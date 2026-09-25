# Changelog

All notable changes to Codex RPC are documented here.

## [0.4.1] - 2026-09-25

### Changed

- Settings window: readable Codex status ("Connected to Codex Desktop"), plan shown once, usage and detail toggles merged into one "Show on profile" row, profile buttons flagged "Available in Watching mode" outside Watching mode. Removed the redundant Apply and Maximize buttons; default height reduced to 620.
- Tray menu: model line uses `·` like the Discord preview, new start-at-login icon, and "Discord: Not connected" replaces "Discord: RPC Disabled".
- Both windows share one color palette and stylesheet (`tauri-ui/common.css`) and theme helper.
- `load_status` and `tray_snapshot` now return the same parsed status fields; the unused `daemon_status` command is removed.

## [0.4.0] - 2026-09-23

### Changed

- Redesigned the settings window and tray popup with visible usage bars. Pro subscriptions show the weekly limit; Plus shows 5-hour and weekly limits when Codex reports them.
- Removed Spark limits, cost and token estimates, and Always on mode from the Tauri app.

### Fixed

- Classify Codex Desktop helpers as Desktop and ignore Codex RPC's own usage probes, preventing false CLI + Desktop presence.
- Keep one Tauri instance across launches and release the instance lock after a crash.
- Validate the daemon lock before stopping a process from `stop.bat`.
- Avoid a slow process lookup when the Node daemon checks its own instance lock.

## [0.3.18] - 2026-05-10

### Changed

- Buttons panel and Watching-mode hint now flag the **Discord Nitro requirement** for custom RPC buttons to render on the user's profile.

## [0.3.17] - 2026-05-10

### Added

- **Always-on mode**: a new `Always on` toggle in Settings keeps Discord RPC active and rate-limit counters refreshing even when no `codex.exe` is running locally. Useful for third-party agents (Hermes Agent, etc.) that draw from your shared agentic usage limit. Discord shows `Monitoring Codex usage` / `Watching Codex usage` (in TV mode) with the configured model and live rate limits. Tauri only — the standalone Node CLI build is unchanged.

## [0.3.16] - 2026-05-10

### Added

- Two new Settings toggles under **Display** — `Effort` and `Credits` — to hide the reasoning effort label and the credits-remaining counter from status bar and Discord state independently.

## [0.3.15] - 2026-05-10

### Changed

- Tray menu trimmed to **Settings · 5h · Week · Spark 5h · Spark week · Start on Windows · Quit**. Mode selectors and per-toggle visibility items are now configured exclusively from the Settings window.
- The four rate-limit entries are read-only (greyed out) and refresh every ~2s with the live "remaining" percentage parsed from the daemon status file. Mode and Discord usage filters remain unchanged in Settings.

## [0.3.14] - 2026-05-10

### Added

- GPT-5.3-Codex-Spark rate limits (5h + Weekly) parsed from `rateLimitsByLimitId` and surfaced in tray status, Discord state, and large-image tooltip.
- Two new Settings toggles — "Spark 5h" and "Spark wk" — with matching tray menu entries.

### Fixed

- Account usage now appears immediately after Codex launches (no first-request needed). The probe spawns `codex app-server` directly with a JSON-RPC `initialize` + `account/rateLimits/read` handshake instead of `app-server proxy`, which required an existing control socket that the desktop app doesn't expose.
- Spark toggles now persist correctly: `RpcSettings` was duplicated between `daemon.rs` and `main.rs`; only the daemon copy carried the new fields, so saves dropped them silently and the daemon always reverted to defaults.

### Changed

- Settings panel label `RPC mode` → `IPC Mode` to match the underlying Discord IPC mechanism.
- Account-usage probe now caches results for 30 seconds (Rust) / refreshes asynchronously in the background (Node) to avoid spawning a fresh `codex app-server` on every detection tick.

## [0.3.13] - 2026-05-08

### Security

- Codex CLI invocations (Rust daemon + Node detector) now use only absolute trusted paths — bare `codex`/`codex.cmd` candidates removed, preventing PATH/search-order hijacking.
- `reg.exe` invocation on Windows now uses `%SystemRoot%\System32\reg.exe` instead of an unqualified name, preventing binary-planting attacks on the startup-toggle feature.
- macOS release workflow hardened: `contents: write` scoped to upload step only; tag input validated against `vX.Y.Z` format; checkout forced to `refs/tags/` with `persist-credentials: false`.

## [0.3.12] - 2026-05-07

### Fixed

- Local Codex usage refresh (login status) now respects its 60-second cooldown correctly in both the Rust daemon and Node detector.

## [0.3.11] - 2026-05-07

### Added

- Account usage sync via Codex CLI `app-server proxy` JSON-RPC — live rate-limit data is now preferred over local rollout file parsing.

### Fixed

- macOS release workflow added for automated DMG/binary publishing.

## [0.3.10] - 2026-05-03

### Fixed

- Usage reset fallback now trusts post-reset snapshots instead of forcing 5h usage to 100%.

## [0.3.9] - 2026-05-01

### Fixed

- Model and reasoning effort now sync from the latest Codex turn context instead of stale top-level config.

## [0.3.8] - 2026-05-01

### Fixed

- Usage limits now show 100% after their reset time even before Codex writes a fresh rate-limit snapshot.

## [0.3.7] - 2026-04-29

### Fixed

- Tauri desktop daemon now uses the same multi-rollout usage selection as the CLI daemon.

## [0.3.6] - 2026-04-29

### Fixed

- Discord RPC now refreshes when usage percentages change.
- Usage display now prefers global Codex limits over model-specific Spark limits.

## [0.3.5] - 2026-04-29

### Fixed

- Usage limits now fall back across recent Codex rollout logs when the newest CLI/Desktop session has no rate-limit snapshot.

## [0.3.4] - 2026-04-27

### Added

- macOS Tauri build with `.app`, `.dmg`, and portable arm64 binary artifacts.
- macOS Codex process detection for CLI and desktop activity.
- macOS Discord RPC IPC support through Unix `discord-ipc-*` sockets.
- macOS start-at-login support through a LaunchAgent.

### Changed

- Settings and status files now use `~/Library/Application Support/codex-rich-presence` on macOS.
- Build scripts now export platform-specific Tauri binaries and validate the signed macOS app bundle.

## [0.3.3] - 2026-04-27

### Added

- Start on Windows toggle in the tray menu.

## [0.3.2] - 2026-04-26

### Changed

- Refreshed settings UI with glass panels, softer backgrounds, and stronger focus states.

## [0.3.1] - 2026-04-26

### Added

- Native Windows process scanner in the integrated Tauri daemon.
- Live autosave for settings.
- Local Discord RPC preview with Codex logo.
- Preview of Discord buttons in Watching mode.
- Separate 5h and weekly usage visibility toggles.
- Tray quick toggles for all RPC modes.
- Tray quick toggles for 5h/week usage visibility.
- Resizable settings window.
- Neutral dark/light UI palette.

### Changed

- Settings and RPC refresh every 500ms.
- Process scanning remains at 5s by default.
- Settings window now uses a calmer grey-first palette, keeping blue for active and primary controls.

### Removed

- PowerShell/CIM process polling from the Tauri daemon.

### Validation

- `npm run build`
- `npm test` - 76 passed
- `npm audit --json` - 0 vulnerabilities
- `cargo check`
- `npm run tauri:build`
- Runtime smoke test: no `codex-rpc-daemon.exe` sidecar.

## [0.3.0] - 2026-04-26

### Changed

- Integrated the RPC daemon into the Tauri process.
- Removed the daemon sidecar from the shipped Tauri app.
- Kept Discord RPC, status file, and tray behavior inside one main app process.

## [0.2.1] - 2026-04-26

### Changed

- Updated app metadata and process description to `Codex RPC`.

## [0.2.0] - 2026-04-26

### Added

- Tauri settings window.
- Discord RPC button configuration.
- RPC mode selection.
- Usage visibility controls.
- Dark, System, and Light themes.
- NSIS installer artifact.

## [0.1.2] - 2026-04-26

### Fixed

- Tray state now reflects Discord disconnects within one tick.

## [0.1.1] - 2026-04-26

### Added

- Discord RPC connection state in tray/status output.

## [0.1.0] - 2026-04-26

### Added

- Initial public release.
- Codex process detection.
- Discord Rich Presence updates.
- Windows executable packaging.
