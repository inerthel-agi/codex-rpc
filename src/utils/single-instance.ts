import fs from 'node:fs';
import { execFileSync } from 'node:child_process';
import os from 'node:os';
import path from 'node:path';

export interface LockResult {
  acquired: boolean;
  /** When acquired=false, the PID of the existing owner (if known). */
  existingPid: number | null;
  release: () => void;
}

interface LockRecord {
  pid: number;
  startTimeMs: number;
  exe: string;
}

interface ProcessIdentity {
  exe: string;
  startTimeMs: number;
}

const PROCESS_START_TOLERANCE_MS = 30_000;
const POWERSHELL_PATH = path.join(
  process.env.SystemRoot ?? 'C:\\Windows',
  'System32',
  'WindowsPowerShell',
  'v1.0',
  'powershell.exe',
);

/**
 * Lock file holds a JSON record identifying the owner: PID, Node start time,
 * and the Node `execPath`. A forged lock file containing an arbitrary PID
 * is rejected because the record must match the process identity.
 */
export function acquireSingleInstanceLock(appName = 'codex-rich-presence'): LockResult {
  const dir = path.join(
    process.env.LOCALAPPDATA ?? path.join(os.homedir(), 'AppData', 'Local'),
    appName,
  );
  fs.mkdirSync(dir, { recursive: true });
  const lockPath = path.join(dir, 'instance.lock');

  const ownRecord: LockRecord = {
    pid: process.pid,
    startTimeMs: startTimeMs(),
    exe: process.execPath,
  };

  const tryAcquire = (): { fd: number | null; existing: LockRecord | null } => {
    try {
      const fd = fs.openSync(lockPath, 'wx');
      fs.writeSync(fd, JSON.stringify(ownRecord));
      return { fd, existing: null };
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code !== 'EEXIST') throw err;
      return { fd: null, existing: readLockRecord(lockPath) };
    }
  };

  let { fd, existing } = tryAcquire();

  if (!fd && (existing === null || !isOwnerAlive(existing))) {
    try {
      fs.unlinkSync(lockPath);
    } catch {
      /* ignore */
    }
    ({ fd, existing } = tryAcquire());
  }

  if (!fd) {
    return { acquired: false, existingPid: existing?.pid ?? null, release: () => undefined };
  }

  const ownFd = fd;
  let released = false;
  const release = (): void => {
    if (released) return;
    released = true;
    try {
      fs.closeSync(ownFd);
    } catch {
      /* ignore */
    }
    try {
      const current = readLockRecord(lockPath);
      if (
        current &&
        current.pid === ownRecord.pid &&
        current.startTimeMs === ownRecord.startTimeMs
      ) {
        fs.unlinkSync(lockPath);
      }
    } catch {
      /* ignore */
    }
  };

  return { acquired: true, existingPid: null, release };
}

function readLockRecord(lockPath: string): LockRecord | null {
  try {
    const raw = fs.readFileSync(lockPath, 'utf8').trim();
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    if (
      parsed &&
      typeof parsed.pid === 'number' &&
      typeof parsed.startTimeMs === 'number' &&
      typeof parsed.exe === 'string'
    ) {
      return parsed;
    }
    const legacyPid = parseInt(raw, 10);
    if (Number.isFinite(legacyPid)) {
      return { pid: legacyPid, startTimeMs: 0, exe: '' };
    }
    return null;
  } catch {
    return null;
  }
}

function isOwnerAlive(record: LockRecord): boolean {
  if (!pidExists(record.pid)) return false;
  if (!record.exe || record.startTimeMs === 0) return false;

  // Reentrant checks already know this process's identity; avoid a slow CIM query.
  if (record.pid === process.pid) {
    return (
      normalizeExePath(record.exe) === normalizeExePath(process.execPath) &&
      Math.abs(startTimeMs() - record.startTimeMs) <= PROCESS_START_TOLERANCE_MS
    );
  }

  const identity = getProcessIdentity(record.pid);
  if (!identity) return false;

  return (
    normalizeExePath(identity.exe) === normalizeExePath(record.exe) &&
    Math.abs(identity.startTimeMs - record.startTimeMs) <= PROCESS_START_TOLERANCE_MS
  );
}

function pidExists(pid: number): boolean {
  if (!Number.isFinite(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    return (err as NodeJS.ErrnoException).code === 'EPERM';
  }
}

function startTimeMs(): number {
  return Math.floor(Date.now() - process.uptime() * 1000);
}

function getProcessIdentity(pid: number): ProcessIdentity | null {
  if (!Number.isInteger(pid) || pid <= 0) return null;

  if (process.platform !== 'win32') {
    return pid === process.pid ? { exe: process.execPath, startTimeMs: startTimeMs() } : null;
  }

  try {
    const command = [
      `$p = Get-CimInstance Win32_Process -Filter "ProcessId=${pid}"`,
      'if ($null -eq $p) { exit 2 }',
      '$p | Select-Object ProcessId,ExecutablePath,CreationDate | ConvertTo-Json -Compress',
    ].join('; ');
    const raw = execFileSync(POWERSHELL_PATH, ['-NoProfile', '-NonInteractive', '-Command', command], {
      encoding: 'utf8',
      maxBuffer: 128 * 1024,
      timeout: 3000,
      windowsHide: true,
    }).trim();
    if (!raw) return null;

    const parsed = JSON.parse(raw);
    const item = Array.isArray(parsed) ? parsed[0] : parsed;
    const exe = typeof item?.ExecutablePath === 'string' ? item.ExecutablePath : '';
    const parsedStart = parsePowerShellDate(item?.CreationDate);
    if (!exe || parsedStart === null) return null;
    return { exe, startTimeMs: parsedStart };
  } catch {
    return null;
  }
}

function parsePowerShellDate(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (typeof value !== 'string') return null;

  const dotNetMatch = /\/Date\((-?\d+)\)\//.exec(value);
  if (dotNetMatch) {
    const timestamp = Number(dotNetMatch[1]);
    return Number.isFinite(timestamp) ? timestamp : null;
  }

  const dmtfMatch = /^(\d{14})\.(\d{6})([+-]\d{3})$/.exec(value);
  if (dmtfMatch) {
    const [, stamp, micros, offset] = dmtfMatch;
    const utcMs = Date.UTC(
      Number(stamp.slice(0, 4)),
      Number(stamp.slice(4, 6)) - 1,
      Number(stamp.slice(6, 8)),
      Number(stamp.slice(8, 10)),
      Number(stamp.slice(10, 12)),
      Number(stamp.slice(12, 14)),
      Number(micros.slice(0, 3)),
    );
    return utcMs - Number(offset) * 60_000;
  }

  const isoMs = Date.parse(value);
  return Number.isFinite(isoMs) ? isoMs : null;
}

function normalizeExePath(value: string): string {
  return path.normalize(value.replace(/^\\\\\?\\/, '')).toLowerCase();
}
