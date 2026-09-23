import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  formatCodexUsage,
  parseAccountUsageResponse,
  readLatestCodexUsage,
} from '../src/detector/codex-usage';

function tmpRoot(): string {
  return path.join(os.tmpdir(), `codex-usage-test-${process.pid}-${Date.now()}`);
}

describe('readLatestCodexUsage', () => {
  let root: string;
  const futureReset = Math.floor(Date.now() / 1000) + 60 * 60;
  const pastReset = Math.floor(Date.now() / 1000) - 60;

  beforeEach(() => {
    root = tmpRoot();
    fs.mkdirSync(root, { recursive: true });
  });
  afterEach(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  function writeRollout(rel: string, lines: unknown[], mtime = new Date()): void {
    const full = path.join(root, rel);
    fs.mkdirSync(path.dirname(full), { recursive: true });
    fs.writeFileSync(full, lines.map((line) => JSON.stringify(line)).join('\n') + '\n');
    fs.utimesSync(full, mtime, mtime);
  }

  it('extracts latest token_count rate limits', () => {
    writeRollout('2026/04/25/rollout-a.jsonl', [
      { type: 'session_meta', payload: { cwd: 'D:\\repo' } },
      {
        type: 'event_msg',
        payload: {
          type: 'token_count',
          rate_limits: {
            limit_id: 'codex',
            primary: { used_percent: 5, window_minutes: 300, resets_at: futureReset },
            secondary: { used_percent: 19, window_minutes: 10080, resets_at: futureReset },
            credits: { remaining: 0 },
            plan_type: 'plus',
          },
        },
      },
    ]);

    const usage = readLatestCodexUsage(root);
    expect(usage?.limitId).toBe('codex');
    expect(usage?.primary?.usedPercent).toBe(5);
    expect(usage?.secondary?.usedPercent).toBe(19);
    expect(usage?.creditsRemaining).toBe(0);
    expect(formatCodexUsage(usage)).toBe('Usage: 5h 95% left / week 81% left / credits 0');
  });

  it('labels the primary slot after its window when the plan has no 5h limit', () => {
    // Accounts without a 5h window receive the weekly limit in `primary` and no
    // `secondary` at all, so the label must follow window_minutes, not the slot.
    writeRollout('2026/07/25/rollout-weekly.jsonl', [
      {
        type: 'event_msg',
        payload: {
          type: 'token_count',
          rate_limits: {
            limit_id: 'codex',
            primary: { used_percent: 11, window_minutes: 10080, resets_at: futureReset },
            secondary: null,
            credits: { balance: '0' },
            plan_type: 'prolite',
          },
        },
      },
    ]);

    const usage = readLatestCodexUsage(root);
    expect(usage?.primary?.windowMinutes).toBe(10080);
    expect(usage?.secondary).toBeNull();
    expect(formatCodexUsage(usage)).toBe('Usage: week 89% left / credits 0');
  });

  it('returns null when no token_count exists', () => {
    writeRollout('rollout-empty.jsonl', [{ type: 'session_meta', payload: {} }]);
    expect(readLatestCodexUsage(root)).toBeNull();
  });

  it('treats expired reset windows as fully available', () => {
    writeRollout('rollout-reset.jsonl', [
      {
        type: 'event_msg',
        payload: {
          type: 'token_count',
          rate_limits: {
            limit_id: 'codex',
            primary: { used_percent: 12, window_minutes: 300, resets_at: pastReset },
            secondary: { used_percent: 32, window_minutes: 10080, resets_at: futureReset },
          },
        },
      },
    ], new Date((pastReset - 60) * 1000));

    expect(formatCodexUsage(readLatestCodexUsage(root))).toBe(
      'Usage: 5h 100% left / week 68% left',
    );
  });

  it('trusts post-reset snapshots with non-zero usage', () => {
    writeRollout('rollout-post-reset.jsonl', [
      {
        type: 'event_msg',
        payload: {
          type: 'token_count',
          rate_limits: {
            limit_id: 'codex',
            primary: { used_percent: 9, window_minutes: 300, resets_at: pastReset },
            secondary: { used_percent: 45, window_minutes: 10080, resets_at: futureReset },
          },
        },
      },
    ]);

    expect(formatCodexUsage(readLatestCodexUsage(root))).toBe(
      'Usage: 5h 91% left / week 55% left',
    );
  });

  it('falls back to older recent rollouts when latest has no rate limits', () => {
    writeRollout(
      'rollout-with-usage.jsonl',
      [
        {
          type: 'event_msg',
          payload: {
            type: 'token_count',
            rate_limits: {
              primary: { used_percent: 21, window_minutes: 300 },
              secondary: { used_percent: 24, window_minutes: 10080 },
            },
          },
        },
      ],
      new Date(Date.now() - 1000),
    );
    writeRollout('rollout-empty.jsonl', [{ type: 'session_meta', payload: {} }]);

    expect(formatCodexUsage(readLatestCodexUsage(root))).toBe(
      'Usage: 5h 79% left / week 76% left',
    );
  });

  it('keeps codex as the primary group without displaying Spark', () => {
    writeRollout(
      'rollout-global.jsonl',
      [
        {
          type: 'event_msg',
          payload: {
            type: 'token_count',
            rate_limits: {
              limit_id: 'codex',
              primary: { used_percent: 22, window_minutes: 300 },
              secondary: { used_percent: 24, window_minutes: 10080 },
            },
          },
        },
      ],
      new Date(Date.now() - 1000),
    );
    writeRollout('rollout-spark.jsonl', [
      {
        type: 'event_msg',
        payload: {
          type: 'token_count',
          rate_limits: {
            limit_id: 'codex_bengalfox',
            primary: { used_percent: 0, window_minutes: 300 },
            secondary: { used_percent: 0, window_minutes: 10080 },
          },
        },
      },
    ]);

    expect(formatCodexUsage(readLatestCodexUsage(root))).toBe(
      'Usage: 5h 78% left / week 76% left',
    );
  });

  it('parses account app-server rate limits keyed by codex', () => {
    const usage = parseAccountUsageResponse(
      JSON.stringify({
        id: 1,
        result: {
          rateLimits: {
            limitId: 'codex_bengalfox',
            primary: { usedPercent: 0, windowDurationMins: 300 },
            secondary: { usedPercent: 0, windowDurationMins: 10080 },
          },
          rateLimitsByLimitId: {
            codex: {
              limitId: 'codex',
              planType: 'prolite',
              primary: { usedPercent: 9, windowDurationMins: 300, resetsAt: futureReset },
              secondary: { usedPercent: 45, windowDurationMins: 10080, resetsAt: futureReset },
              credits: { balance: '0', hasCredits: false, unlimited: false },
            },
          },
        },
      }),
      Date.now(),
    );

    expect(usage?.limitId).toBe('codex');
    expect(formatCodexUsage(usage)).toBe('Usage: week 55% left / credits 0');
  });

  it('ignores Spark rate limits alongside the codex group', () => {
    const usage = parseAccountUsageResponse(
      JSON.stringify({
        id: 1,
        result: {
          rateLimitsByLimitId: {
            codex: {
              limitId: 'codex',
              primary: { usedPercent: 0, windowDurationMins: 300, resetsAt: futureReset },
              secondary: { usedPercent: 19, windowDurationMins: 10080, resetsAt: futureReset },
              credits: { balance: '0' },
              planType: 'prolite',
            },
            codex_bengalfox: {
              limitId: 'codex_bengalfox',
              limitName: 'GPT-5.3-Codex-Spark',
              primary: { usedPercent: 0, windowDurationMins: 300, resetsAt: futureReset },
              secondary: { usedPercent: 0, windowDurationMins: 10080, resetsAt: futureReset },
            },
          },
        },
      }),
      Date.now(),
    );

    expect(usage?.limitId).toBe('codex');
    expect(formatCodexUsage(usage)).toBe(
      'Usage: week 81% left / credits 0',
    );
  });

  it('labels weekly-only account limits as week and hides Spark', () => {
    const usage = parseAccountUsageResponse(
      JSON.stringify({
        id: 1,
        result: {
          rateLimitsByLimitId: {
            codex: {
              limitId: 'codex',
              primary: { usedPercent: 11, windowDurationMins: 10080, resetsAt: futureReset },
              credits: { balance: '0' },
              planType: 'prolite',
            },
            codex_bengalfox: {
              limitId: 'codex_bengalfox',
              limitName: 'GPT-5.3-Codex-Spark',
              primary: { usedPercent: 0, windowDurationMins: 10080, resetsAt: futureReset },
            },
          },
        },
      }),
      Date.now(),
    );

    expect(formatCodexUsage(usage)).toBe(
      'Usage: week 89% left / credits 0',
    );
  });

  it('ignores Spark rollout entries and retains the codex limits', () => {
    writeRollout(
      'rollout-codex.jsonl',
      [
        {
          type: 'event_msg',
          payload: {
            type: 'token_count',
            rate_limits: {
              limit_id: 'codex',
              primary: { used_percent: 22, window_minutes: 300 },
              secondary: { used_percent: 24, window_minutes: 10080 },
            },
          },
        },
      ],
      new Date(Date.now() - 1000),
    );
    writeRollout('rollout-spark.jsonl', [
      {
        type: 'event_msg',
        payload: {
          type: 'token_count',
          rate_limits: {
            limit_id: 'codex_bengalfox',
            primary: { used_percent: 0, window_minutes: 300 },
            secondary: { used_percent: 0, window_minutes: 10080 },
          },
        },
      },
    ]);

    const usage = readLatestCodexUsage(root);
    expect(usage?.limitId).toBe('codex');
  });
});
