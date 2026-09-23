import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export interface CodexConfig {
  model: string | null;
  effort: string | null;
  serviceTier: string | null;
}

const DEFAULT_PATH = path.join(os.homedir(), '.codex', 'config.toml');
const SESSIONS_ROOT = path.join(os.homedir(), '.codex', 'sessions');
const READ_TAIL_BYTES = 1024 * 1024;

/**
 * Minimal TOML extractor — we only care about three top-level string keys.
 * Running a full parser isn't worth the dep: the file can contain arbitrary
 * nested tables we explicitly don't want to touch.
 */
export function parseCodexConfig(tomlContent: string): CodexConfig {
  return {
    model: extractTopLevelString(tomlContent, 'model'),
    effort: extractTopLevelString(tomlContent, 'model_reasoning_effort'),
    serviceTier: extractTopLevelString(tomlContent, 'service_tier'),
  };
}

export function readCodexConfig(
  configPath: string = DEFAULT_PATH,
  sessionsRoot: string = SESSIONS_ROOT,
): CodexConfig | null {
  let fileConfig: CodexConfig | null = null;
  let fileMtimeMs = 0;
  try {
    const raw = fs.readFileSync(configPath, 'utf8');
    fileConfig = parseCodexConfig(raw);
    fileMtimeMs = fs.statSync(configPath).mtimeMs;
  } catch {
    fileConfig = null;
  }
  let runtimeConfig = readLatestTurnContextConfig(sessionsRoot);
  // A `/model` change rewrites config.toml immediately, but the last
  // turn_context in the rollout log still carries the previous model until the
  // next turn starts. When the config file is newer than the last recorded
  // turn, trust the file — otherwise stale runtime data wins for minutes.
  if (runtimeConfig && fileConfig && fileMtimeMs > runtimeConfig.timestampMs) {
    runtimeConfig = null;
  }
  if (!fileConfig && !runtimeConfig) return null;
  return {
    model: runtimeConfig?.model ?? fileConfig?.model ?? null,
    effort: runtimeConfig?.effort ?? fileConfig?.effort ?? null,
    serviceTier: fileConfig?.serviceTier ?? null,
  };
}

/**
 * Extracts `key = "value"` or `key = 'value'` that appears at the top of the file,
 * before any `[section]` header. Ignores matches inside nested tables.
 */
function extractTopLevelString(toml: string, key: string): string | null {
  const lines = toml.split(/\r?\n/);
  const keyPattern = new RegExp(`^\\s*${escapeRegex(key)}\\s*=\\s*(.+?)\\s*$`);
  for (const line of lines) {
    if (/^\s*\[/.test(line)) break; // reached first [section] — stop.
    const match = line.match(keyPattern);
    if (!match) continue;
    const value = match[1].trim();
    const quoted = value.match(/^"((?:[^"\\]|\\.)*)"$/) ?? value.match(/^'([^']*)'$/);
    if (quoted) return quoted[1];
    return value;
  }
  return null;
}

function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

interface TurnContextConfig {
  model: string | null;
  effort: string | null;
  /** When the turn started — from the rollout line's own timestamp, falling back to file mtime. */
  timestampMs: number;
}

function readLatestTurnContextConfig(root: string): TurnContextConfig | null {
  const files = findRecentRolloutFiles(root, 24 * 60 * 60 * 1000);
  for (const file of files) {
    const lines = readTailLines(file.path);
    if (!lines) continue;
    for (let i = lines.length - 1; i >= 0; i--) {
      const config = parseTurnContextLine(lines[i], file.mtimeMs);
      if (config) return config;
    }
  }
  return null;
}

function parseTurnContextLine(line: string, fallbackMtimeMs: number): TurnContextConfig | null {
  try {
    const obj = JSON.parse(line);
    if (obj?.type !== 'turn_context') return null;
    const payload = obj.payload;
    const model = typeof payload?.model === 'string' ? payload.model : null;
    const effort = typeof payload?.effort === 'string' ? payload.effort : null;
    if (!model && !effort) return null;
    const parsed = typeof obj.timestamp === 'string' ? Date.parse(obj.timestamp) : NaN;
    return { model, effort, timestampMs: Number.isNaN(parsed) ? fallbackMtimeMs : parsed };
  } catch {
    return null;
  }
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
      if (now - mtimeMs <= maxAgeMs) files.push({ path: full, mtimeMs });
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

/** Normalize CLI-style shorthand to user-facing label (mirrors Codex app UI). */
export function formatEffort(effort: string | null): string | null {
  if (!effort) return null;
  const map: Record<string, string> = {
    minimal: 'Minimal',
    low: 'Low',
    medium: 'Medium',
    high: 'High',
    xhigh: 'Extra High',
    'extra-high': 'Extra High',
    max: 'Max',
    ultra: 'Ultra',
  };
  return map[effort.toLowerCase()] ?? effort;
}

/** e.g. `gpt-5.4` → `GPT-5.4`, `gpt-5.3-codex` → `GPT-5.3-Codex`. */
export function formatModel(model: string | null): string | null {
  if (!model) return null;
  return model
    .split('-')
    .map((segment, i) => {
      if (segment.length === 0) return segment;
      if (i === 0 && /^[a-z]+$/.test(segment)) return segment.toUpperCase();
      if (/^[a-z]/.test(segment)) return segment.charAt(0).toUpperCase() + segment.slice(1);
      return segment;
    })
    .join('-');
}
