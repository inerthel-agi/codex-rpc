import { describe, expect, it } from 'vitest';
import { buildPresence } from '../src/rpc/presence-builder';
import type { DetectionResult } from '../src/detector/state';

function mk(
  state: DetectionResult['state'],
  isoStart: string | null,
  codex: DetectionResult['codex'] = null,
  session: DetectionResult['session'] = null,
  usage: DetectionResult['usage'] = null,
): DetectionResult {
  return {
    state,
    startedAt: isoStart ? new Date(isoStart) : null,
    processCounts: { cli: 0, app: 0, unknown: 0 },
    codex,
    session,
    usage,
  };
}

describe('buildPresence', () => {
  it('idle → null (RPC will clear)', () => {
    expect(buildPresence(mk('idle', null))).toBeNull();
  });

  it('cli — falls back to static string when no codex config', () => {
    expect(buildPresence(mk('cli', '2026-04-18T10:00:00Z'))).toEqual({
      name: 'Codex',
      type: 0,
      details: 'Coding with Codex CLI',
      state: 'Terminal session active',
      largeImageKey: 'codex_logo',
      largeImageText: 'OpenAI Codex',
      smallImageKey: 'cli_badge',
      smallImageText: 'Codex CLI',
      startTimestamp: 1776506400,
      instance: false,
    });
  });

  it('cli — shows model + effort when config present', () => {
    const p = buildPresence(
      mk('cli', '2026-04-18T10:00:00Z', {
        model: 'gpt-5.4',
        effort: 'xhigh',
        serviceTier: 'fast',
      }),
    );
    expect(p?.details).toBe('Coding with Codex CLI');
    expect(p?.state).toBe('GPT-5.4 · Extra High');
  });

  it('cli — appends repo name to details when session snapshot exists', () => {
    const p = buildPresence(
      mk(
        'cli',
        '2026-04-18T10:00:00Z',
        { model: 'gpt-5.4', effort: 'xhigh', serviceTier: null },
        {
          cwd: 'D:\\repos\\codex-rich-presence',
          originator: 'codex-tui',
          repoName: 'codex-rich-presence',
          lastActivityMs: Date.now(),
        },
      ),
    );
    expect(p?.details).toBe('Coding with Codex CLI · codex-rich-presence');
  });

  it('app — shows model alone when effort missing', () => {
    const p = buildPresence(
      mk('app', '2026-04-18T10:00:00Z', {
        model: 'gpt-5.3-codex',
        effort: null,
        serviceTier: null,
      }),
    );
    expect(p?.details).toBe('Using Codex');
    expect(p?.state).toBe('GPT-5.3-Codex');
  });

  it('both — details mentions both, state uses model+effort', () => {
    const p = buildPresence(
      mk('both', '2026-04-18T10:00:00Z', {
        model: 'gpt-5.4',
        effort: 'high',
        serviceTier: null,
      }),
    );
    expect(p?.details).toBe('Coding with Codex (CLI + Desktop)');
    expect(p?.state).toBe('GPT-5.4 · High');
  });

  it('truncates the repo name and always stays within the details cap', () => {
    const huge = 'x'.repeat(200);
    const p = buildPresence(
      mk('cli', '2026-04-18T10:00:00Z', null, {
        cwd: `D:\\${huge}`,
        originator: 'codex-tui',
        repoName: huge,
        lastActivityMs: Date.now(),
      }),
    );
    expect(p?.details.length).toBeLessThanOrEqual(96);
    expect(p?.details.startsWith('Coding with Codex CLI')).toBe(true);
  });

  it('omits startTimestamp when startedAt is null', () => {
    const p = buildPresence(mk('cli', null));
    expect(p?.startTimestamp).toBeUndefined();
  });

  it('emits small image badge for the active mode', () => {
    const p = buildPresence(mk('both', '2026-04-18T10:00:00Z'));
    expect(p?.smallImageKey).toBe('combo_badge');
    expect(p?.smallImageText).toBe('CLI + Desktop');
  });

  it('adds compact usage to state and image tooltip', () => {
    const p = buildPresence(
      mk(
        'cli',
        '2026-04-18T10:00:00Z',
        { model: 'gpt-5.5', effort: 'high', serviceTier: null },
        null,
        {
          limitId: 'codex',
          primary: { usedPercent: 5, windowMinutes: 300, resetsAt: null },
          secondary: { usedPercent: 19, windowMinutes: 10080, resetsAt: null },
          creditsRemaining: 0,
          planType: 'plus',
          lastActivityMs: Date.now(),
        },
      ),
    );
    expect(p?.state).toBe('GPT-5.5 · High · 5h 95% · week 81%');
    expect(p?.largeImageText).toBe('OpenAI Codex · 5h 95% · week 81%');
  });

  it('shows only the weekly limit for Pro', () => {
    const p = buildPresence(
      mk(
        'cli',
        '2026-04-18T10:00:00Z',
        { model: 'gpt-5.5', effort: 'high', serviceTier: null },
        null,
        {
          limitId: 'codex',
          primary: { usedPercent: 5, windowMinutes: 300, resetsAt: null },
          secondary: { usedPercent: 19, windowMinutes: 10080, resetsAt: null },
          creditsRemaining: 0,
          planType: 'pro',
          lastActivityMs: Date.now(),
        },
      ),
    );
    expect(p?.largeImageText).toBe('OpenAI Codex · week 81%');
  });

  it('adds optional RPC buttons only in TV mode', () => {
    const p = buildPresence(mk('cli', '2026-04-18T10:00:00Z'), [
      { label: 'Open Codex', url: 'https://chatgpt.com/codex' },
    ]);
    expect(p?.buttons).toBeUndefined();

    const tv = buildPresence(
      mk('cli', '2026-04-18T10:00:00Z'),
      [{ label: 'Open Codex', url: 'https://chatgpt.com/codex' }],
      'watching',
    );
    expect(tv?.type).toBe(3);
    expect(tv?.name).toBe('Codex');
    expect(tv?.details).toBe('Watching Codex CLI');
    expect(tv?.buttons).toEqual([
      { label: 'Open Codex', url: 'https://chatgpt.com/codex' },
    ]);
  });

  it('supports listening and competing activity types', () => {
    expect(buildPresence(mk('cli', '2026-04-18T10:00:00Z'), [], 'listening')?.type).toBe(2);
    expect(buildPresence(mk('cli', '2026-04-18T10:00:00Z'), [], 'competing')?.type).toBe(5);
  });
});
