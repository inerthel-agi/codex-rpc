import type { ProcessSnapshot } from './process-scanner';

export type ProcessKind = 'cli' | 'app' | 'unknown';

// Rule 1 — canonical path used by the user's npm global install.
const CANONICAL_CLI_REGEX =
  /\\node_modules\\@openai\\codex\\node_modules\\@openai\\codex-win32-x64\\vendor\\x86_64-pc-windows-msvc\\codex\\codex\.exe$/i;

// Rule 2 — any install variant (pnpm, yarn, bun, global, monorepo) under @openai\codex.
const NPM_CODEX_REGEX = /\\node_modules\\@openai\\codex\\/i;

// Rule 3 — parent process is a shell/terminal/IDE → CLI (safety net when path is unknown).
const SHELL_PARENTS = new Set<string>([
  'cmd.exe',
  'powershell.exe',
  'pwsh.exe',
  'windowsterminal.exe',
  'wt.exe',
  'bash.exe',
  'code.exe',
  'cursor.exe',
  'conemu.exe',
  'conemu64.exe',
  'conemuc.exe',
  'conemuc64.exe',
  'alacritty.exe',
  'tabby.exe',
  'fluent-terminal.exe',
  'hyper.exe',
]);

export interface ClassificationContext {
  process: ProcessSnapshot;
}

export interface Classification {
  kind: ProcessKind;
  /** which rule fired — useful for logging */
  rule: 1 | 2 | 3 | 4 | 0;
}

export function classify(ctx: ClassificationContext): Classification {
  const { process: p } = ctx;
  const exe = p.executablePath ?? '';
  const parent = p.parentName?.toLowerCase() ?? null;

  if (p.owner === 'ignored') return { kind: 'unknown', rule: 0 };
  if (p.owner === 'app' || /\/(?:openai|programs|program files)\/codex\//i.test(exe.replace(/\\/g, '/'))) {
    return { kind: 'app', rule: 4 };
  }

  if (exe && CANONICAL_CLI_REGEX.test(exe)) {
    return { kind: 'cli', rule: 1 };
  }

  if (exe && NPM_CODEX_REGEX.test(exe)) {
    return { kind: 'cli', rule: 2 };
  }

  if (parent && SHELL_PARENTS.has(parent)) {
    return { kind: 'cli', rule: 3 };
  }

  if (exe) {
    return { kind: 'app', rule: 4 };
  }

  // No path, no useful parent — keep as unknown to avoid false positives.
  return { kind: 'unknown', rule: 0 };
}
