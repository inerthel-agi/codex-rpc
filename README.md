# Codex RPC

Codex RPC shows local Codex activity and account usage in a Windows or macOS tray app and sends the activity to Discord Rich Presence. It supports Codex CLI and Codex Desktop.

## Requirements

- Windows or macOS with a running Discord desktop client.
- Windows: WebView2 Runtime. Source builds also need Node.js 22, Rust/Cargo, Visual Studio 2022 Build Tools with MSVC v143, and the Windows SDK.
- macOS source builds: Node.js 22, Rust/Cargo, and Xcode Command Line Tools.
- TODO: Document the minimum supported Windows, macOS, and Rust versions.

## Install

Download the installer for your platform from [GitHub Releases](https://github.com/inerthel-agi/codex-rpc/releases/latest), then run it. The Windows release also provides a portable executable.

To build from source on Windows:

```powershell
npm ci
npm run tauri:build:windows
```

To build from source on macOS:

```bash
npm ci
npm run tauri:build:macos
```

The Windows build places the portable executable at `bin/codex-rich-presence.exe`. The macOS build creates an app bundle and DMG under `src-tauri/target/release/bundle/`.

## Usage

Start Codex RPC from the installer or launch the portable Windows build:

```powershell
.\bin\codex-rich-presence.exe
```

The app starts in the system tray. Left-click its icon to open settings. Right-click to see usage, startup control, and Quit. Select the Discord activity mode in settings. Profile buttons are sent only in Watching mode.

Codex RPC shows the weekly allowance for Pro subscriptions and both 5-hour and weekly allowances for Plus when Codex reports those limits. The display for other plans follows the 5-hour and weekly windows returned by Codex.

## Configuration

The settings window saves the Discord mode, profile buttons, usage visibility, and theme. Settings are stored in `%LOCALAPPDATA%\codex-rich-presence\rpc-buttons.json` on Windows and `~/Library/Application Support/codex-rich-presence/rpc-buttons.json` on macOS.

| Variable | Type | Default | Effect |
| --- | --- | --- | --- |
| `DISCORD_CLIENT_ID` | string | Bundled Discord application ID | Overrides the Discord application ID. |
| `SCAN_INTERVAL_MS` | integer, milliseconds | `5000` | Process scan interval; minimum `2000`. |
| `IDLE_GRACE_MS` | integer, milliseconds | `10000` | Delay before clearing activity after Codex exits. |

The separate Node CLI under `src/` is retained for development. It does not provide every Tauri setting.

## Limitations

- Discord must be running for Rich Presence to appear.
- The macOS app is ad-hoc signed and is not notarized. If Gatekeeper blocks the download, clear its quarantine attribute with `xattr -cr "/Applications/Codex RPC.app"` after reviewing the app.
- Usage windows other than 5 hours and one week are not displayed.

## License

MIT. See [LICENSE](LICENSE).
