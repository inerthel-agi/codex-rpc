import { spawn } from 'node:child_process';
import { getLogger } from '../utils/logger';

export interface RawProcess {
  owner?: 'app' | 'ignored';
  processId: number;
  parentProcessId: number;
  executablePath: string | null;
  commandLine: string | null;
  creationDate: Date | null;
  parentName: string | null;
}

/**
 * One-shot PowerShell pipeline: fetches every Win32_Process, joins each
 * codex.exe to its parent's Name via a hashtable, and emits JSON.
 * Replaces what used to be two sequential Get-CimInstance calls.
 */
const PS_COMMAND =
  '$procs = Get-CimInstance Win32_Process;' +
  '$index = @{};' +
  'foreach ($p in $procs) { $index[[int]$p.ProcessId] = $p.Name };' +
  '$parents = @{}; foreach ($p in $procs) { $parents[[int]$p.ProcessId] = $p };' +
  "$procs | Where-Object { $_.Name -eq 'codex.exe' } | ForEach-Object {" +
  '$owner = $null; $ancestor = [int]$_.ParentProcessId;' +
  'for ($i = 0; $i -lt $procs.Count; $i++) {' +
  `  if ($ancestor -eq ${process.pid}) { $owner = 'ignored'; break };` +
  '  $p = $parents[$ancestor]; if ($null -eq $p) { break };' +
  "  if ($p.Name -in @('codex-rich-presence.exe', 'codex_rich_presence_tray.exe')) { $owner = 'ignored'; break };" +
  "  if ($p.Name -eq 'ChatGPT.exe') { $owner = 'app'; break };" +
  '  $ancestor = [int]$p.ParentProcessId;' +
  '}; [PSCustomObject]@{' +
  '  Owner           = $owner;' +
  '  ProcessId       = $_.ProcessId;' +
  '  ParentProcessId = $_.ParentProcessId;' +
  '  ExecutablePath  = $_.ExecutablePath;' +
  '  CommandLine     = $_.CommandLine;' +
  '  CreationDate    = $_.CreationDate;' +
  '  ParentName      = $index[[int]$_.ParentProcessId];' +
  '} } | ConvertTo-Json -Compress -Depth 3';

/**
 * Absolute path to Windows PowerShell — bypasses PATH lookup so a malicious
 * `powershell.exe` planted earlier in the user PATH cannot be invoked.
 * Falls back to `C:\Windows\...` when SystemRoot is unset (shouldn't happen
 * on a normal logon).
 */
function resolvePowershell(): string {
  const systemRoot = process.env.SystemRoot ?? 'C:\\Windows';
  return `${systemRoot}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe`;
}

const PS_ARGS = ['-NoProfile', '-NonInteractive', '-Command', PS_COMMAND];

/** Hard cap on PowerShell stdout (2 MB) — protects against runaway output. */
const PS_STDOUT_CAP_BYTES = 2 * 1024 * 1024;

/** WMI CIM datetime comes through as `/Date(1713435600000)/` via ConvertTo-Json. */
function parseCimDate(raw: unknown): Date | null {
  if (typeof raw !== 'string') return null;
  const match = raw.match(/\/Date\((\d+)\)\//);
  if (!match) return null;
  const ms = parseInt(match[1], 10);
  return Number.isFinite(ms) ? new Date(ms) : null;
}

function normalize(entry: Record<string, unknown>): RawProcess | null {
  const pid = entry.ProcessId;
  if (typeof pid !== 'number') return null;
  return {
    owner: entry.Owner === 'app' || entry.Owner === 'ignored' ? entry.Owner : undefined,
    processId: pid,
    parentProcessId: typeof entry.ParentProcessId === 'number' ? entry.ParentProcessId : -1,
    executablePath: typeof entry.ExecutablePath === 'string' ? entry.ExecutablePath : null,
    commandLine: typeof entry.CommandLine === 'string' ? entry.CommandLine : null,
    creationDate: parseCimDate(entry.CreationDate),
    parentName: typeof entry.ParentName === 'string' ? entry.ParentName : null,
  };
}

export function parseScanOutput(stdout: string): RawProcess[] {
  const trimmed = stdout.trim();
  if (!trimmed) return [];
  let data: unknown;
  try {
    data = JSON.parse(trimmed);
  } catch {
    return [];
  }
  const entries = Array.isArray(data) ? data : [data];
  return entries
    .filter((e): e is Record<string, unknown> => typeof e === 'object' && e !== null)
    .map(normalize)
    .filter((e): e is RawProcess => e !== null);
}

export interface ProcessSnapshot {
  owner?: 'app' | 'ignored';
  processId: number;
  parentProcessId: number;
  parentName: string | null;
  executablePath: string | null;
  commandLine: string | null;
  creationDate: Date | null;
}

export interface ScannerOptions {
  timeoutMs?: number;
}

export async function scanCodexProcesses(options: ScannerOptions = {}): Promise<ProcessSnapshot[]> {
  const log = getLogger();
  const timeoutMs = options.timeoutMs ?? 8000;
  const raw = await runPowershell(resolvePowershell(), PS_ARGS, timeoutMs);
  if (raw === null) return [];

  const processes = parseScanOutput(raw);
  if (processes.length === 0) return [];

  log.debug({ count: processes.length }, 'scanner: codex.exe processes found');

  return processes.map((p) => ({
    owner: p.owner,
    processId: p.processId,
    parentProcessId: p.parentProcessId,
    parentName: p.parentName,
    executablePath: p.executablePath,
    commandLine: p.commandLine,
    creationDate: p.creationDate,
  }));
}

function runPowershell(exe: string, args: string[], timeoutMs: number): Promise<string | null> {
  return new Promise((resolve) => {
    const log = getLogger();
    const stdoutChunks: Buffer[] = [];
    let stdoutBytes = 0;
    let stderr = '';
    let settled = false;
    let overflowed = false;

    const child = spawn(exe, args, {
      windowsHide: true,
      // Reduced environment: we don't need user-controlled vars inside PS.
      env: {
        SystemRoot: process.env.SystemRoot,
        SystemDrive: process.env.SystemDrive,
        ComSpec: process.env.ComSpec,
        PATHEXT: process.env.PATHEXT,
      } as NodeJS.ProcessEnv,
    });

    const finish = (result: string | null, reason?: string): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try {
        child.kill('SIGKILL');
      } catch {
        /* already gone */
      }
      if (reason) log.warn({ reason }, 'scanner: powershell aborted');
      resolve(result);
    };

    const timer = setTimeout(() => finish(null, 'timeout'), timeoutMs);

    child.stdout.on('data', (chunk: Buffer) => {
      if (overflowed) return;
      stdoutBytes += chunk.length;
      if (stdoutBytes > PS_STDOUT_CAP_BYTES) {
        overflowed = true;
        finish(null, 'stdout-cap-exceeded');
        return;
      }
      stdoutChunks.push(chunk);
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk.toString('utf8');
      if (stderr.length > 8192) stderr = stderr.slice(-8192);
    });
    child.on('error', (err) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      log.error({ err: err.message }, 'scanner: powershell spawn failed');
      resolve(null);
    });
    child.on('close', (code) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (code !== 0) {
        log.warn({ code, stderr: stderr.slice(0, 500) }, 'scanner: powershell non-zero exit');
        resolve(null);
        return;
      }
      resolve(Buffer.concat(stdoutChunks).toString('utf8'));
    });
  });
}
