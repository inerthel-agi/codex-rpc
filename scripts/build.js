#!/usr/bin/env node
'use strict';

// Builds the release binary, then packages it for the current platform:
// Windows: bin/codex-rich-presence.exe and an NSIS installer.
// macOS: an ad-hoc signed app bundle, a DMG and the bare binary in bin/.

const fs = require('node:fs');
const os = require('node:os');
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
const APP_ID = 'io.github.inerthel-agi.codex-rich-presence';

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

function packageMacos() {
  const arch = os.arch() === 'arm64' ? 'aarch64' : 'x64';
  const binary = path.join(RELEASE, BINARY);
  const app = path.join(BUNDLE, 'macos', `${PRODUCT}.app`);
  fs.rmSync(app, { recursive: true, force: true });
  copy(binary, path.join(app, 'Contents', 'MacOS', BINARY));
  fs.chmodSync(path.join(app, 'Contents', 'MacOS', BINARY), 0o755);
  copy(path.join(ROOT, 'assets', 'icon.icns'), path.join(app, 'Contents', 'Resources', 'icon.icns'));
  const plist = {
    CFBundleName: PRODUCT,
    CFBundleDisplayName: PRODUCT,
    CFBundleIdentifier: APP_ID,
    CFBundleExecutable: BINARY,
    CFBundleIconFile: 'icon.icns',
    CFBundlePackageType: 'APPL',
    CFBundleInfoDictionaryVersion: '6.0',
    CFBundleShortVersionString: version,
    CFBundleVersion: version,
    LSMinimumSystemVersion: '10.13',
  };
  const entries = Object.entries(plist)
    .map(([key, value]) => `  <key>${key}</key>\n  <string>${value}</string>`)
    .join('\n');
  fs.writeFileSync(
    path.join(app, 'Contents', 'Info.plist'),
    `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
${entries}
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
`,
  );

  run('codesign', ['--force', '--deep', '--sign', process.env.APPLE_SIGNING_IDENTITY || '-', app]);
  run('codesign', ['--verify', '--deep', '--strict', '--verbose=2', app]);

  const staging = path.join(BUNDLE, 'macos-dmg-staging');
  const dmg = path.join(BUNDLE, 'dmg', `${PRODUCT}_${version}_${arch}.dmg`);
  fs.rmSync(staging, { recursive: true, force: true });
  fs.mkdirSync(staging, { recursive: true });
  fs.cpSync(app, path.join(staging, `${PRODUCT}.app`), { recursive: true });
  try {
    fs.symlinkSync('/Applications', path.join(staging, 'Applications'));
  } catch {
    // The symlink is a convenience in Finder; the DMG remains usable without it.
  }
  fs.mkdirSync(path.dirname(dmg), { recursive: true });
  run('hdiutil', ['create', '-volname', PRODUCT, '-srcfolder', staging, '-ov', '-format', 'UDZO', dmg]);
  fs.rmSync(staging, { recursive: true, force: true });
  copy(binary, path.join(ROOT, 'bin', `${BINARY}-macos-${os.arch() === 'arm64' ? 'arm64' : 'x64'}`));
}

run('cargo', ['build', '--release', '--locked', '--manifest-path', path.join(ROOT, 'src-tauri', 'Cargo.toml')]);
if (process.platform === 'win32') {
  packageWindows();
} else if (process.platform === 'darwin') {
  packageMacos();
} else {
  copy(path.join(RELEASE, BINARY), path.join(ROOT, 'bin', `${BINARY}-${process.platform}-${os.arch()}`));
}
