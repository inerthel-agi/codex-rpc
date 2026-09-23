import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { formatEffort, formatModel, parseCodexConfig, readCodexConfig } from '../src/detector/codex-config';

function tmpRoot(): string {
  return path.join(os.tmpdir(), `codex-config-test-${process.pid}-${Date.now()}`);
}

describe('parseCodexConfig', () => {
  let root: string;

  beforeEach(() => {
    root = tmpRoot();
    fs.mkdirSync(root, { recursive: true });
  });
  afterEach(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('extracts top-level model, effort, service_tier', () => {
    const toml = [
      'model = "gpt-5.4"',
      'model_reasoning_effort = "xhigh"',
      'service_tier = "fast"',
      '',
      '[projects.\'C:\\Users\\foo\']',
      'trust_level = "trusted"',
    ].join('\n');
    expect(parseCodexConfig(toml)).toEqual({
      model: 'gpt-5.4',
      effort: 'xhigh',
      serviceTier: 'fast',
    });
  });

  it('accepts single-quoted literals', () => {
    const toml = "model = 'gpt-5.3-codex'";
    expect(parseCodexConfig(toml).model).toBe('gpt-5.3-codex');
  });

  it('returns null for missing keys', () => {
    expect(parseCodexConfig('')).toEqual({ model: null, effort: null, serviceTier: null });
  });

  it('ignores keys nested under a section', () => {
    const toml = ['[nested]', 'model = "should-be-ignored"'].join('\n');
    expect(parseCodexConfig(toml).model).toBeNull();
  });

  it('stops scanning once a section header is reached', () => {
    const toml = [
      'service_tier = "fast"',
      '[projects.\'p\']',
      'model = "nope"',
    ].join('\n');
    const c = parseCodexConfig(toml);
    expect(c.serviceTier).toBe('fast');
    expect(c.model).toBeNull();
  });

  it('overlays active turn context model and effort', () => {
    const configPath = path.join(root, 'config.toml');
    const sessionsRoot = path.join(root, 'sessions');
    const rolloutPath = path.join(sessionsRoot, 'rollout-a.jsonl');
    fs.writeFileSync(
      configPath,
      ['model = "gpt-5.5"', 'model_reasoning_effort = "medium"'].join('\n'),
    );
    fs.mkdirSync(sessionsRoot, { recursive: true });
    fs.writeFileSync(
      rolloutPath,
      JSON.stringify({
        type: 'turn_context',
        payload: { model: 'gpt-5.4', effort: 'high' },
      }) + '\n',
    );

    expect(readCodexConfig(configPath, sessionsRoot)).toEqual({
      model: 'gpt-5.4',
      effort: 'high',
      serviceTier: null,
    });
  });

  it('prefers config.toml when it is newer than the last turn context', () => {
    const configPath = path.join(root, 'config.toml');
    const sessionsRoot = path.join(root, 'sessions');
    const rolloutPath = path.join(sessionsRoot, 'rollout-a.jsonl');
    fs.mkdirSync(sessionsRoot, { recursive: true });
    fs.writeFileSync(
      rolloutPath,
      JSON.stringify({
        timestamp: '2026-07-09T18:00:59.694Z',
        type: 'turn_context',
        payload: { model: 'gpt-5.6-sol', effort: 'ultra' },
      }) + '\n',
    );
    // /model rewrote config.toml two minutes after the last turn started.
    fs.writeFileSync(
      configPath,
      ['model = "gpt-5.6-terra"', 'model_reasoning_effort = "max"'].join('\n'),
    );
    const configMtime = new Date('2026-07-09T18:02:58.000Z');
    fs.utimesSync(configPath, configMtime, configMtime);

    expect(readCodexConfig(configPath, sessionsRoot)).toEqual({
      model: 'gpt-5.6-terra',
      effort: 'max',
      serviceTier: null,
    });
  });
});

describe('formatEffort', () => {
  it('maps CLI shorthand to app labels', () => {
    expect(formatEffort('xhigh')).toBe('Extra High');
    expect(formatEffort('high')).toBe('High');
    expect(formatEffort('medium')).toBe('Medium');
    expect(formatEffort('low')).toBe('Low');
    expect(formatEffort('minimal')).toBe('Minimal');
    expect(formatEffort('ultra')).toBe('Ultra');
    expect(formatEffort('max')).toBe('Max');
  });

  it('falls back to the raw value when unknown', () => {
    expect(formatEffort('turbo')).toBe('turbo');
  });

  it('returns null for null', () => {
    expect(formatEffort(null)).toBeNull();
  });
});

describe('formatModel', () => {
  it('uppercases model prefix', () => {
    expect(formatModel('gpt-5.4')).toBe('GPT-5.4');
    expect(formatModel('gpt-5.3-codex')).toBe('GPT-5.3-Codex');
    expect(formatModel('gpt-5.2-codex-mini')).toBe('GPT-5.2-Codex-Mini');
    expect(formatModel('gpt-5.6-sol')).toBe('GPT-5.6-Sol');
    expect(formatModel('gpt-5.6-terra')).toBe('GPT-5.6-Terra');
    expect(formatModel('gpt-5.6-luna')).toBe('GPT-5.6-Luna');
  });

  it('null passthrough', () => {
    expect(formatModel(null)).toBeNull();
  });
});
