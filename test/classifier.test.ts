import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { classify } from '../src/detector/classifier';
import { parseScanOutput, type RawProcess } from '../src/detector/process-scanner';

function loadFixture(name: string): string {
  return fs.readFileSync(path.join(__dirname, 'fixtures', name), 'utf8');
}

function toSnapshot(raw: RawProcess, parentName: string | null) {
  return {
    processId: raw.processId,
    parentProcessId: raw.parentProcessId,
    parentName,
    executablePath: raw.executablePath,
    commandLine: raw.commandLine,
    creationDate: raw.creationDate,
  };
}

describe('classifier', () => {
  it('keeps Desktop helpers in Desktop mode and excludes RPC probes', () => {
    const [raw] = parseScanOutput(loadFixture('wmi-npm-global.json'));
    const process = toSnapshot(raw, 'powershell.exe');
    expect(classify({ process: { ...process, owner: 'app' } }).kind).toBe('app');
    expect(classify({ process: { ...process, owner: 'ignored' } }).kind).toBe('unknown');
    expect(classify({ process }).kind).toBe('cli');
    expect(classify({ process: { ...process, executablePath: 'C:\\Users\\test\\AppData\\Local\\OpenAI\\Codex\\bin\\version\\codex.exe' } }).kind).toBe('app');
    expect(parseScanOutput(JSON.stringify({ ProcessId: 42, Owner: 'app' }))[0].owner).toBe('app');
  });
  it('rule 1: canonical npm-global vendor path → cli', () => {
    const [raw] = parseScanOutput(loadFixture('wmi-npm-global.json'));
    const r = classify({ process: toSnapshot(raw, 'cmd.exe') });
    expect(r.kind).toBe('cli');
    expect(r.rule).toBe(1);
  });

  it('rule 2: pnpm install variant → cli', () => {
    const [raw] = parseScanOutput(loadFixture('wmi-pnpm.json'));
    const r = classify({ process: toSnapshot(raw, 'powershell.exe') });
    expect(r.kind).toBe('cli');
    expect(r.rule).toBe(2);
  });

  it('rule 2: bun global install → cli', () => {
    const [raw] = parseScanOutput(loadFixture('wmi-bun-global.json'));
    const r = classify({ process: toSnapshot(raw, 'wt.exe') });
    expect(r.kind).toBe('cli');
    expect(r.rule).toBe(2);
  });

  it('rule 3: unknown path but shell parent → cli', () => {
    const [raw] = parseScanOutput(loadFixture('wmi-ambiguous.json'));
    const r = classify({ process: toSnapshot(raw, 'WindowsTerminal.exe') });
    expect(r.kind).toBe('cli');
    expect(r.rule).toBe(3);
  });

  it('rule 3: VSCode terminal parent → cli', () => {
    const [raw] = parseScanOutput(loadFixture('wmi-ambiguous.json'));
    const r = classify({ process: toSnapshot({ ...raw, executablePath: 'C:\\whatever\\codex.exe' }, 'Code.exe') });
    expect(r.kind).toBe('cli');
    expect(r.rule).toBe(3);
  });

  it('rule 4: LocalAppData Programs path with explorer parent → app', () => {
    const [raw] = parseScanOutput(loadFixture('wmi-app-localappdata.json'));
    const r = classify({ process: toSnapshot(raw, 'explorer.exe') });
    expect(r.kind).toBe('app');
    expect(r.rule).toBe(4);
  });

  it('rule 4: Program Files path → app', () => {
    const [raw] = parseScanOutput(loadFixture('wmi-app-programfiles.json'));
    const r = classify({ process: toSnapshot(raw, 'explorer.exe') });
    expect(r.kind).toBe('app');
    expect(r.rule).toBe(4);
  });

  it('no path, no shell parent → unknown (not surfaced)', () => {
    const [raw] = parseScanOutput(loadFixture('wmi-ambiguous.json'));
    const r = classify({ process: toSnapshot(raw, 'some-random-parent.exe') });
    expect(r.kind).toBe('unknown');
    expect(r.rule).toBe(0);
  });
});

describe('parseScanOutput', () => {
  it('accepts an object (single process) or an array', () => {
    const obj = parseScanOutput(loadFixture('wmi-app-programfiles.json'));
    const arr = parseScanOutput(loadFixture('wmi-npm-global.json'));
    expect(obj).toHaveLength(1);
    expect(arr).toHaveLength(1);
  });

  it('parses CIM /Date(ms)/ into Date', () => {
    const [raw] = parseScanOutput(loadFixture('wmi-npm-global.json'));
    expect(raw.creationDate).toBeInstanceOf(Date);
    expect(raw.creationDate?.getTime()).toBe(1713435600000);
  });

  it('returns [] for empty input', () => {
    expect(parseScanOutput('')).toEqual([]);
    expect(parseScanOutput('   ')).toEqual([]);
  });
});
