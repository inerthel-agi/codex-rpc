import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';

const onWindows = process.platform === 'win32' ? describe : describe.skip;
const script = path.resolve('scripts/stop-daemon.ps1');
const powershell = path.join(
  process.env.SystemRoot ?? 'C:\\Windows',
  'System32',
  'WindowsPowerShell',
  'v1.0',
  'powershell.exe',
);

onWindows('stop daemon lock validation', () => {
  it('rejects command text and an unrelated live PID without stopping it', () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codex-rpc-stop-test-'));
    const lock = path.join(dir, 'instance.lock');
    const marker = path.join(dir, 'injected.txt');
    try {
      for (const [value, error] of [
        [`999999 & echo injected>${marker}`, 'Invalid PID'],
        [JSON.stringify({ pid: process.pid, exe: process.execPath, startTimeMs: Date.now() }),
          'Lock PID does not identify this daemon'],
      ]) {
        fs.writeFileSync(lock, value);
        const result = spawnSync(powershell, [
          '-NoProfile', '-NonInteractive', '-File', script, '-LockPath', lock,
        ], { encoding: 'utf8', timeout: 30_000 });
        expect(result.status).toBe(1);
        expect(result.stderr).toContain(error);
        expect(fs.existsSync(lock)).toBe(true);
        expect(fs.existsSync(marker)).toBe(false);
      }
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  }, 45_000);
});
