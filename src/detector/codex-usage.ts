import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';

export interface CodexLimitSnapshot {
  usedPercent: number;
  windowMinutes: number | null;
  resetsAt: Date | null;
  observedAtMs: number;
}

export interface CodexUsageSnapshot {
  limitId: string | null;
  primary: CodexLimitSnapshot | null;
  secondary: CodexLimitSnapshot | null;
  creditsRemaining: number | null;
  planType: string | null;
  lastActivityMs: number;
}

const SESSIONS_ROOT = path.join(os.homedir(), '.codex', 'sessions');
const READ_TAIL_BYTES = 256 * 1024;
const ACCOUNT_USAGE_CACHE_MS = 30 * 1000;
const LOCAL_USAGE_REFRESH_MS = 60 * 1000;
let accountUsageCache: { checkedAt: number; usage: CodexUsageSnapshot | null } | null = null;
let lastLocalUsageRefreshMs = 0;

export function readLatestCodexUsage(
  root: string = SESSIONS_ROOT,
  maxAgeMs: number = 24 * 60 * 60 * 1000,
): CodexUsageSnapshot | null {
  if (root === SESSIONS_ROOT) {
    const accountUsage = readCodexAccountUsage();
    if (accountUsage) return accountUsage;
    refreshLocalCodexUsage();
  }

  for (const file of findRecentRolloutFiles(root, maxAgeMs)) {
    const lines = readTailLines(file.path);
    if (!lines) continue;
    for (let i = lines.length - 1; i >= 0; i--) {
      const usage = parseUsageLine(lines[i], file.mtimeMs);
      if (usage && (!usage.limitId || usage.limitId === 'codex')) return usage;
    }
  }
  return null;
}

function refreshLocalCodexUsage(): void {
  const now = Date.now();
  if (now - lastLocalUsageRefreshMs < LOCAL_USAGE_REFRESH_MS) return;
  lastLocalUsageRefreshMs = now;

  for (const command of codexCommandCandidates()) {
    const result = spawnSync(command, ['login', 'status'], {
      encoding: 'utf8',
      timeout: 2500,
      windowsHide: true,
    });
    if (result.status === 0) return;
  }
}

// app-server reads stdin asynchronously; spawnSync can't keep stdin alive long
// enough for the rate-limits response to land. We kick off a background refresh
// instead and serve the previously-cached value synchronously.
let accountUsageRefreshInFlight = false;

function readCodexAccountUsage(): CodexUsageSnapshot | null {
  const now = Date.now();
  if (!accountUsageCache || now - accountUsageCache.checkedAt >= ACCOUNT_USAGE_CACHE_MS) {
    scheduleAccountUsageRefresh();
  }
  return accountUsageCache?.usage ?? null;
}

function scheduleAccountUsageRefresh(): void {
  if (accountUsageRefreshInFlight) return;
  accountUsageRefreshInFlight = true;
  refreshCodexAccountUsage()
    .then((usage) => {
      accountUsageCache = { checkedAt: Date.now(), usage };
    })
    .catch(() => {
      accountUsageCache = { checkedAt: Date.now(), usage: accountUsageCache?.usage ?? null };
    })
    .finally(() => {
      accountUsageRefreshInFlight = false;
    });
}

async function refreshCodexAccountUsage(): Promise<CodexUsageSnapshot | null> {
  const initRequest =
    '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"clientInfo":{"name":"codex-rpc","version":"0"}}}\n';
  // Codex 0.144+ rejects requests sent before the `initialized` notification.
  const initializedNotification = '{"jsonrpc":"2.0","method":"initialized"}\n';
  const readRequest =
    '{"jsonrpc":"2.0","id":1,"method":"account/rateLimits/read","params":null}\n';

  for (const command of codexCommandCandidates()) {
    const stdout = await runAppServerProbe(
      command,
      initRequest + initializedNotification + readRequest,
    );
    if (!stdout) continue;
    const usage = parseAccountUsageResponse(stdout, Date.now());
    if (usage) return usage;
  }
  return null;
}

function runAppServerProbe(command: string, request: string): Promise<string | null> {
  return new Promise((resolve) => {
    let child;
    try {
      child = spawn(command, ['app-server'], {
        stdio: ['pipe', 'pipe', 'ignore'],
        // .cmd shims on Windows need cmd.exe (Node 22+ rejects them with EINVAL otherwise).
        shell: process.platform === 'win32' && command.toLowerCase().endsWith('.cmd'),
        windowsHide: true,
      });
    } catch {
      resolve(null);
      return;
    }

    let buffer = '';
    let settled = false;
    const finish = (value: string | null): void => {
      if (settled) return;
      settled = true;
      try {
        child.kill();
      } catch {
        /* ignore */
      }
      resolve(value);
    };

    const timeout = setTimeout(() => finish(buffer || null), 5000);

    child.on('error', () => {
      clearTimeout(timeout);
      finish(null);
    });
    child.on('exit', () => {
      clearTimeout(timeout);
      finish(buffer || null);
    });
    child.stdout?.on('data', (chunk: Buffer) => {
      buffer += chunk.toString('utf8');
      if (lineWithIdOne(buffer)) {
        clearTimeout(timeout);
        finish(buffer);
      }
    });

    if (child.stdin) {
      child.stdin.on('error', () => {
        /* ignore broken pipe after kill */
      });
      child.stdin.write(request);
    }
  });
}

function lineWithIdOne(text: string): boolean {
  const newlineIdx = text.lastIndexOf('\n');
  if (newlineIdx <= 0) return false;
  const lines = text.slice(0, newlineIdx).split(/\r?\n/);
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed.startsWith('{')) continue;
    try {
      const obj = JSON.parse(trimmed);
      if (obj?.id === 1 || obj?.id === '1') return true;
    } catch {
      continue;
    }
  }
  return false;
}

function codexCommandCandidates(): string[] {
  let candidates: string[];
  if (process.platform === 'win32') {
    const appData = process.env.APPDATA;
    const programFiles = process.env.ProgramFiles;
    candidates = [
      appData ? path.join(appData, 'npm', 'codex.cmd') : null,
      programFiles ? path.join(programFiles, 'nodejs', 'codex.cmd') : null,
    ].filter((value): value is string => Boolean(value));
  } else {
    const home = process.env.HOME;
    candidates = [
      home ? path.join(home, '.local', 'bin', 'codex') : null,
      '/opt/homebrew/bin/codex',
      '/usr/local/bin/codex',
      '/usr/bin/codex',
    ].filter((value): value is string => Boolean(value));
  }
  return candidates.filter((p) => {
    try {
      return fs.statSync(p).isFile();
    } catch {
      return false;
    }
  });
}

export function parseAccountUsageResponse(
  raw: string,
  observedAtMs: number,
): CodexUsageSnapshot | null {
  for (const line of raw.split(/\r?\n/)) {
    if (!line.trim()) continue;
    try {
      const msg = JSON.parse(line);
      if ((msg?.id !== 1 && msg?.id !== '1') || !msg.result) continue;
      return parseAccountUsagePayload(msg.result, observedAtMs);
    } catch {
      continue;
    }
  }
  return null;
}

function parseAccountUsagePayload(
  payload: Record<string, unknown>,
  observedAtMs: number,
): CodexUsageSnapshot | null {
  const byLimitId =
    payload.rateLimitsByLimitId && typeof payload.rateLimitsByLimitId === 'object'
      ? (payload.rateLimitsByLimitId as Record<string, unknown>)
      : null;
  const codexEntry = byLimitId?.codex;
  const limits = codexEntry ?? payload.rateLimits;
  if (!limits || typeof limits !== 'object') return null;

  const record = limits as Record<string, unknown>;
  return {
    limitId: typeof record.limitId === 'string' ? record.limitId : null,
    primary: parseAccountLimit(record.primary, observedAtMs),
    secondary: parseAccountLimit(record.secondary, observedAtMs),
    creditsRemaining: parseCredits(record.credits),
    planType: typeof record.planType === 'string' ? record.planType : null,
    lastActivityMs: observedAtMs,
  };
}

export function visibleUsageLimits(usage: CodexUsageSnapshot | null): { label: string; limit: CodexLimitSnapshot }[] {
  if (!usage) return [];
  const pro = usage.planType?.toLowerCase().startsWith('pro');
  const limits = [
    { label: usageLabel(usage.primary, '5h'), limit: usage.primary },
    { label: usageLabel(usage.secondary, 'week'), limit: usage.secondary },
  ];
  return limits.filter((entry): entry is { label: string; limit: CodexLimitSnapshot } =>
    entry.limit !== null && (entry.label === 'week' || (!pro && entry.label === '5h')));
}

export function formatCodexUsage(usage: CodexUsageSnapshot | null): string | null {
  if (!usage) return null;
  const parts = visibleUsageLimits(usage).map(({ label, limit }) => formatLimit(label, limit));
  if (usage.creditsRemaining !== null) parts.push('credits ' + usage.creditsRemaining);
  return parts.length ? 'Usage: ' + parts.join(' / ') : null;
}

function parseUsageLine(line: string, lastActivityMs: number): CodexUsageSnapshot | null {
  try {
    const obj = JSON.parse(line);
    if (obj?.type !== 'event_msg' || obj.payload?.type !== 'token_count') return null;
    const limits = obj.payload.rate_limits;
    if (!limits || typeof limits !== 'object') return null;
    return {
      limitId: typeof limits.limit_id === 'string' ? limits.limit_id : null,
      primary: parseLimit(limits.primary, lastActivityMs),
      secondary: parseLimit(limits.secondary, lastActivityMs),
      creditsRemaining: parseCredits(limits.credits),
      planType: typeof limits.plan_type === 'string' ? limits.plan_type : null,
      lastActivityMs,
    };
  } catch {
    return null;
  }
}

function parseLimit(value: unknown, observedAtMs: number): CodexLimitSnapshot | null {
  if (!value || typeof value !== 'object') return null;
  const record = value as Record<string, unknown>;
  const usedPercent = typeof record.used_percent === 'number' ? record.used_percent : null;
  if (usedPercent === null) return null;
  const windowMinutes =
    typeof record.window_minutes === 'number' ? record.window_minutes : null;
  const resetsAt =
    typeof record.resets_at === 'number' ? new Date(record.resets_at * 1000) : null;
  return { usedPercent, windowMinutes, resetsAt, observedAtMs };
}

function parseAccountLimit(value: unknown, observedAtMs: number): CodexLimitSnapshot | null {
  if (!value || typeof value !== 'object') return null;
  const record = value as Record<string, unknown>;
  const usedPercent = typeof record.usedPercent === 'number' ? record.usedPercent : null;
  if (usedPercent === null) return null;
  const windowMinutes =
    typeof record.windowDurationMins === 'number' ? record.windowDurationMins : null;
  const resetsAt =
    typeof record.resetsAt === 'number' ? new Date(record.resetsAt * 1000) : null;
  return { usedPercent, windowMinutes, resetsAt, observedAtMs };
}

function parseCredits(value: unknown): number | null {
  if (typeof value === 'number') return value;
  if (!value || typeof value !== 'object') return null;
  const record = value as Record<string, unknown>;
  if (typeof record.balance === 'string') {
    const parsed = Number(record.balance);
    if (Number.isFinite(parsed)) return parsed;
  }
  if (typeof record.remaining === 'number') return record.remaining;
  if (typeof record.balance === 'number') return record.balance;
  return null;
}

function formatLimit(label: string, limit: CodexLimitSnapshot | null): string | null {
  if (!limit) return null;
  const remaining = remainingPercent(limit);
  return `${label} ${remaining}% left`;
}

/**
 * Names a limit after the window Codex actually reports. Accounts without a 5h
 * window now receive the weekly limit in the `primary` slot, so the slot alone can
 * no longer name the row. `fallback` covers older rollout lines with no window.
 */
export function usageLabel(limit: CodexLimitSnapshot | null, fallback: string): string {
  const minutes = limit?.windowMinutes ?? null;
  if (minutes === null) return fallback;
  if (minutes >= 7 * 24 * 60) return 'week';
  if (minutes >= 24 * 60) return `${Math.floor(minutes / (24 * 60))}d`;
  if (minutes >= 60) return `${Math.floor(minutes / 60)}h`;
  return `${minutes}m`;
}

export function remainingPercent(limit: CodexLimitSnapshot): number {
  if (
    limit.resetsAt &&
    limit.resetsAt.getTime() <= Date.now() &&
    limit.observedAtMs < limit.resetsAt.getTime()
  ) {
    return 100;
  }
  return Math.max(0, Math.round(100 - limit.usedPercent));
}

function findRecentRolloutFiles(
  root: string,
  maxAgeMs: number,
): { path: string; mtimeMs: number }[] {
  const files: { path: string; mtimeMs: number }[] = [];
  const now = Date.now();

  const walk = (dir: string): void => {
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(full);
        continue;
      }
      if (!entry.isFile() || !entry.name.startsWith('rollout-') || !entry.name.endsWith('.jsonl')) {
        continue;
      }
      let mtimeMs: number;
      try {
        mtimeMs = fs.statSync(full).mtimeMs;
      } catch {
        continue;
      }
      if (now - mtimeMs > maxAgeMs) continue;
      files.push({ path: full, mtimeMs });
    }
  };

  walk(root);
  return files.sort((a, b) => b.mtimeMs - a.mtimeMs);
}

function readTailLines(filePath: string): string[] | null {
  try {
    const stat = fs.statSync(filePath);
    const length = Math.min(stat.size, READ_TAIL_BYTES);
    const offset = stat.size - length;
    const fd = fs.openSync(filePath, 'r');
    try {
      const buf = Buffer.alloc(length);
      fs.readSync(fd, buf, 0, length, offset);
      const text = buf.toString('utf8');
      const lines = text.split(/\r?\n/).filter((line) => line.length > 0);
      return offset > 0 ? lines.slice(1) : lines;
    } finally {
      fs.closeSync(fd);
    }
  } catch {
    return null;
  }
}
