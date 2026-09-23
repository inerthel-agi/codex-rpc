import { formatEffort, formatModel } from '../detector/codex-config';
import type { RpcActivityMode, RpcButtonConfig } from '../config';
import { remainingPercent, visibleUsageLimits } from '../detector/codex-usage';
import type { CodexLimitSnapshot } from '../detector/codex-usage';
import type { DetectionResult, PresenceState } from '../detector/state';

const ACTIVITY_TYPE_PLAYING = 0;
const ACTIVITY_TYPE_LISTENING = 2;
const ACTIVITY_TYPE_WATCHING = 3;
const ACTIVITY_TYPE_COMPETING = 5;

/**
 * `config.toml` and session rollouts are user-local but still considered
 * lower-trust. Before any string lands in Discord we strip control chars,
 * normalize Unicode, and cap length to prevent UX abuse (homoglyph spoofing,
 * RTL overrides, overly long banners).
 */
const CONTROL_CHAR_REGEX = /[\u0000-\u001F\u007F-\u009F\u200B-\u200F\u202A-\u202E\u2060-\u2069\uFEFF]/g;
const MAX_FIELD_LENGTH = 48;

function sanitizeField(raw: string | null | undefined, maxLen = MAX_FIELD_LENGTH): string | null {
  if (!raw) return null;
  const cleaned = raw
    .normalize('NFC')
    .replace(CONTROL_CHAR_REGEX, '')
    .replace(/\s+/g, ' ')
    .trim();
  if (!cleaned) return null;
  return cleaned.length > maxLen ? cleaned.slice(0, maxLen - 1) + '…' : cleaned;
}

export interface PresencePayload {
  name: string;
  type: number;
  details: string;
  state: string;
  startTimestamp?: number;
  largeImageKey: string;
  largeImageText: string;
  smallImageKey: string;
  smallImageText: string;
  buttons?: RpcButtonConfig[];
  instance?: boolean;
}

const LARGE_IMAGE_KEY = 'codex_logo';
const LARGE_IMAGE_TEXT = 'OpenAI Codex';
const SMALL_IMAGE: Record<Exclude<PresenceState, 'idle'>, { key: string; text: string }> = {
  cli: { key: 'cli_badge', text: 'Codex CLI' },
  app: { key: 'app_badge', text: 'Codex Desktop' },
  both: { key: 'combo_badge', text: 'CLI + Desktop' },
};

const FALLBACK_STATE: Record<Exclude<PresenceState, 'idle'>, string> = {
  cli: 'Terminal session active',
  app: 'Desktop session',
  both: 'CLI + Desktop',
};

const DETAILS: Record<Exclude<PresenceState, 'idle'>, string> = {
  cli: 'Coding with Codex CLI',
  app: 'Using Codex',
  both: 'Coding with Codex (CLI + Desktop)',
};

// Discord's `details` field caps around 128 chars; repo names beyond this read poorly.
const MAX_DETAILS_LENGTH = 96;

export function buildPresence(
  result: DetectionResult,
  buttons: RpcButtonConfig[] = [],
  mode: RpcActivityMode = 'playing',
): PresencePayload | null {
  if (result.state === 'idle') return null;
  const payload: PresencePayload = {
    name: 'Codex',
    type: activityType(mode),
    details: buildDetails(result, mode),
    state: buildStateLine(result),
    largeImageKey: LARGE_IMAGE_KEY,
    largeImageText: buildLargeImageText(result),
    smallImageKey: SMALL_IMAGE[result.state].key,
    smallImageText: SMALL_IMAGE[result.state].text,
    instance: false,
  };
  if (mode === 'watching' && buttons.length > 0) payload.buttons = buttons.slice(0, 2);
  if (result.startedAt) {
    payload.startTimestamp = Math.floor(result.startedAt.getTime() / 1000);
  }
  return payload;
}

function activityType(mode: RpcActivityMode): number {
  if (mode === 'watching') return ACTIVITY_TYPE_WATCHING;
  if (mode === 'listening') return ACTIVITY_TYPE_LISTENING;
  if (mode === 'competing') return ACTIVITY_TYPE_COMPETING;
  return ACTIVITY_TYPE_PLAYING;
}

function buildDetails(result: DetectionResult, mode: RpcActivityMode): string {
  if (result.state === 'idle') return '';
  const base =
    mode === 'watching' ? DETAILS[result.state].replace(/^Coding with/, 'Watching') : DETAILS[result.state];
  const repo = sanitizeField(result.session?.repoName, 32);
  if (!repo) return base;
  const suffix = ` · ${repo}`;
  if (base.length + suffix.length <= MAX_DETAILS_LENGTH) return base + suffix;
  return base;
}

function buildStateLine(result: DetectionResult): string {
  if (result.state === 'idle') return '';
  const model = sanitizeField(formatModel(result.codex?.model ?? null), 24);
  const effort = sanitizeField(formatEffort(result.codex?.effort ?? null), 16);
  const parts: string[] = [];
  if (model) parts.push(model);
  if (effort) parts.push(effort);
  const base = parts.length > 0 ? parts.join(' · ') : FALLBACK_STATE[result.state];
  const usage = compactUsageParts(result);
  for (let count = usage.length; count >= 0; count--) {
    const suffix = usage.slice(0, count).join(' · ');
    const candidate = suffix ? `${base} · ${suffix}` : base;
    if (candidate.length <= MAX_FIELD_LENGTH) return candidate;
  }
  return base.length <= MAX_FIELD_LENGTH ? base : base.slice(0, MAX_FIELD_LENGTH - 1) + '…';
}

function buildLargeImageText(result: DetectionResult): string {
  const usage = compactUsageParts(result);
  if (usage.length === 0) return LARGE_IMAGE_TEXT;
  return sanitizeField(`${LARGE_IMAGE_TEXT} · ${usage.join(' · ')}`) ?? LARGE_IMAGE_TEXT;
}

function compactUsageParts(result: DetectionResult): string[] {
  return visibleUsageLimits(result.usage).map(({ label, limit }) => compactLimit(label, limit)!);
}

function compactLimit(label: string, limit: CodexLimitSnapshot | null): string | null {
  if (!limit) return null;
  const remaining = remainingPercent(limit);
  return `${label} ${remaining}%`;
}
