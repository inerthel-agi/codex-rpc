#!/usr/bin/env node
'use strict';

// Builds the release binary, then packages it: bin/codex-rich-presence.exe and an NSIS installer.

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const ROOT = path.resolve(__dirname, '..');
const RELEASE = path.join(ROOT, 'src-tauri', 'target', 'release');
const BUNDLE = path.join(RELEASE, 'bundle');
// The version lives in Cargo.toml only, so the app and its packages always agree.
const version = /^version\s*=\s*"([^"]+)"/m.exec(
  fs.readFileSync(path.join(ROOT, 'src-tauri', 'Cargo.toml'), 'utf8'),
)[1];
const PRODUCT = 'Codex RPC';
const BINARY = 'codex-rich-presence';

function run(command, args) {
  const result = spawnSync(command, args, { cwd: ROOT, stdio: 'inherit', shell: false });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}

function copy(source, target) {
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.copyFileSync(source, target);
  console.log(`[build] ${path.relative(ROOT, target)}`);
}

function findMakensis() {
  const candidates = [
    'makensis',
    path.join(process.env['ProgramFiles(x86)'] || '', 'NSIS', 'makensis.exe'),
    path.join(process.env.LOCALAPPDATA || '', 'tauri', 'NSIS', 'makensis.exe'),
  ];
  return candidates.find((candidate) => spawnSync(candidate, ['/VERSION']).status === 0);
}

function packageWindows() {
  const exe = path.join(RELEASE, `${BINARY}.exe`);
  copy(exe, path.join(ROOT, 'bin', `${BINARY}.exe`));
  const makensis = findMakensis();
  if (!makensis) {
    console.warn('[build] NSIS (makensis) not found: the installer was not built.');
    return;
  }
  const setup = path.join(BUNDLE, 'nsis', `${PRODUCT}_${version}_x64-setup.exe`);
  fs.mkdirSync(path.dirname(setup), { recursive: true });
  run(makensis, [
    '/V2',
    `/DVERSION=${version}`,
    `/DSOURCE=${exe}`,
    `/DOUTFILE=${setup}`,
    path.join(ROOT, 'installer', 'windows.nsi'),
  ]);
  console.log(`[build] ${path.relative(ROOT, setup)}`);
}

run('cargo', ['build', '--release', '--locked', '--manifest-path', path.join(ROOT, 'src-tauri', 'Cargo.toml')]);
packageWindows();
