<p align="center">
  <img src="assets/codex_logo.png" width="140" alt="Codex RPC logo">
</p>

<h1 align="center">Codex RPC</h1>

<p align="center">
  Windows and macOS tray Discord Rich Presence for OpenAI Codex.
  Detects Codex CLI, editor-launched Codex sessions, and Codex desktop, shows
  model/effort/Fast mode, and lets you control Discord activity from a local
  Tauri settings window.
</p>

<p align="center">
  <a href="#features">Features</a> |
  <a href="#install">Install</a> |
  <a href="#usage">Usage</a> |
  <a href="#development">Development</a> |
  <a href="CHANGELOG.md">Changelog</a>
</p>

## Features

- Native Windows/macOS tray app built with Tauri and Rust.
- Single process app: no `codex-rpc-daemon.exe` sidecar.
- Native Windows process scan for `codex.exe`, no PowerShell polling.
- macOS process scan for Codex CLI and Codex desktop.
- Detects Codex CLI vs Codex desktop.
- Discord RPC modes: Playing, Watching, Listening, Competing.
- Discord buttons support in Watching mode.
- Optional 5h, weekly, Spark 5h, Spark weekly, credits, cost, and token display toggles.
- Fast mode display when Codex is running with `service_tier = "fast"`.
- Always-on mode for monitoring usage when no local Codex process is running.
- Live autosave for settings.
- Local preview of the Discord activity, including button preview.
- Tray usage summary and start-at-login toggle.
- Dark, System, and Light themes.
- Resizable settings window.

## Install

Download the latest release:

https://github.com/inerthel-agi/codex-rpc/releases/latest

Recommended asset:

- `Codex RPC_0.3.21_x64-setup.exe`
- `Codex RPC_0.3.21_aarch64.dmg` or `Codex RPC_0.3.21_x64.dmg` on macOS

Portable asset:

- `codex-rich-presence.exe`
- `codex-rich-presence-macos-arm64` or `codex-rich-presence-macos-x64`

Run the app once. It starts in the system tray. Left-click the tray icon to open
settings, or right-click for quick toggles and Quit.

### macOS: "app is damaged" or "cannot be opened"

The macOS build is ad-hoc signed but not notarized by Apple, so Gatekeeper
blocks it after download. Clear the quarantine flag once:

```bash
xattr -cr "/Applications/Codex RPC.app"
```

Alternatively, right-click the app and choose Open, or allow it under
System Settings → Privacy & Security → "Open Anyway".

## Usage

The settings window controls:

- RPC mode: Playing, Watching, Listening, Competing.
- Two optional Discord buttons. Buttons are sent only in Watching mode.
- 5h, weekly, Spark 5h, Spark weekly, and credits visibility.
- Current-project and all-projects cost/token visibility.
- Always-on monitoring when no Codex process is running.
- Theme.

The tray menu controls:

- Open settings.
- Live 5h, weekly, Spark 5h, and Spark weekly usage.
- Start on Windows.
- Quit.

## Runtime Paths

The Tauri app is the primary shipped app. It runs the tray, process scanner,
settings window, and Discord IPC in one Rust/Tauri process.

The Node entrypoint under `src/` is kept for legacy CLI/dev workflows and tests.
It does not carry every Tauri-only settings surface.

Settings are saved under:

```text
%LOCALAPPDATA%\codex-rich-presence\rpc-buttons.json
~/Library/Application Support/codex-rich-presence/rpc-buttons.json
```

The live status file is:

```text
%LOCALAPPDATA%\codex-rich-presence\status.txt
~/Library/Application Support/codex-rich-presence/status.txt
```

## Detection

Codex CLI and Codex desktop can have similar process names, so Codex RPC does
not rely on process name alone.

Detection uses:

- executable path;
- parent process name;
- native Windows process creation time.

CLI is detected when the path contains `node_modules/@openai/codex`, or when
the parent is a terminal/editor shell such as `cmd.exe`, `pwsh.exe`, `wt.exe`,
`Code.exe`, `cursor.exe`, `zsh`, `bash`, Terminal, iTerm2, Warp, Alacritty,
Hyper, Tabby, or ConEmu.

Everything else with a valid Codex executable path is treated as Codex desktop.

## Codex Metadata

Codex RPC reads local Codex files only:

- `~\.codex\config.toml` for model and reasoning effort.
- `~\.codex\config.toml` for Fast mode via `service_tier = "fast"`.
- `~\.codex\sessions\**\rollout-*.jsonl` for repo name, active turn context,
  usage snapshots, and cost/token estimates.

No Codex data is sent anywhere except the Discord Rich Presence payload through
the local Discord IPC pipe.

## Environment

Optional overrides:

| Variable | Default | Description |
| --- | --- | --- |
| `DISCORD_CLIENT_ID` | bundled app id | Override Discord Application ID. |
| `SCAN_INTERVAL_MS` | `5000` | Codex process scan interval. Minimum `2000`. |
| `IDLE_GRACE_MS` | `10000` | Keep last active state before clearing RPC. |

Settings refresh every 500ms so UI changes apply quickly. Process scanning stays
at 5s by default to avoid unnecessary polling.

## Development

Requirements:

- Node.js 22+
- Rust/Cargo via rustup
- Windows: Visual Studio 2022 Build Tools with MSVC v143, Windows SDK, WebView2 Runtime
- macOS: Xcode Command Line Tools

Install dependencies:

```bash
npm install
```

Run checks:

```bash
npm run build
npm test
cd src-tauri
cargo check
```

Build Windows app:

```bash
npm run tauri:build:windows
```

Outputs:

```text
bin\codex-rich-presence.exe
src-tauri\target\release\bundle\nsis\Codex RPC_0.3.21_x64-setup.exe
```

Build macOS app:

```bash
npm run tauri:build:macos
```

Outputs:

```text
bin/codex-rich-presence-macos-arm64
src-tauri/target/release/bundle/macos/Codex RPC.app
src-tauri/target/release/bundle/dmg/Codex RPC_0.3.21_aarch64.dmg
```

## Security

- Button URLs are limited to `http://` and `https://`.
- Discord IPC frame size is capped.
- RPC text fields are sanitized before they reach Discord.
- Process scanning uses native Windows APIs instead of shelling out.
- The app only reads local Codex config/session files and local Discord IPC.

## License

MIT
