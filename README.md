# GamePause for LM Studio

[![CI](https://github.com/Vkuparin/gamepause-lmstudio/actions/workflows/ci.yml/badge.svg)](https://github.com/Vkuparin/gamepause-lmstudio/actions/workflows/ci.yml)
[![MIT](https://img.shields.io/badge/license-MIT-155e75.svg)](LICENSE)
[![Windows x64](https://img.shields.io/badge/platform-Windows_x64-155e75.svg)](docs/INSTALLATION.md)

**Give your games room. Bring your local AI back afterward.**

GamePause is a small native Rust app for Windows. It notices when a game starts, saves the models currently loaded in LM Studio, unloads them, and restores them afterward. The model name can change: there is no fixed model list to maintain.

## Features

- Discovers Steam, Epic, Xbox, EA, Ubisoft Connect, and Battle.net installations. Local inventories refresh every 30 seconds. Xbox package discovery refreshes every five minutes and defers during gaming.
- Checks running processes every two seconds using Windows APIs and cached executable paths. Desktop shortcuts work too.
- Captures loaded LLM and embedding instances, exact model selections, identifiers, TTL policies, and complete load configuration. Restoration verifies the settings.
- Stops LM Studio's HTTP server during gaming by default to prevent HTTP clients from immediately loading models again.
- Waits 30 seconds after the last game exits. Starting another game cancels the delay; alt-tabbing keeps AI paused.
- Keeps a durable recovery journal through partial failures and restarts.
- Verifies the full pause/restore round-trip on demand: capture, unload, confirm the server emptied, restore, and field-compare the read-back settings — from the dashboard, tray, or `GamePauseCLI.exe --verify`.
- Stable one-line-per-item CLI output for scripting: `--status` (key=value state), `--games` (name/launcher/path), plus `--doctor` and `--verify`; works while a GUI instance is running.
- Includes a native dashboard, searchable games, live settings, tray controls, automatic startup at Windows sign-in, a per-user installer, and a portable ZIP.

No Python runtime, administrator service, telemetry, recursive game scans, or permanent model-name configuration. Model control uses localhost.

## Get started

1. Download the **Setup.exe** from [Releases](https://github.com/Vkuparin/gamepause-lmstudio/releases) and install it. Startup at sign-in is checked by default.
2. Keep LM Studio open with the models you want available. Its `lms` CLI must be installed; GamePause finds it automatically. See [Installation](docs/INSTALLATION.md).
3. Launch games normally. **Automatic pausing is on by default.** GamePause discovers supported launcher installations, saves and unloads your currently loaded models, and restores them after gaming.

Left-click the tray icon or open GamePause from Start to see its dashboard. The **Games** list shows what was discovered and what is running. Newly installed games become available automatically; no observation-mode trial, tooltip inspection, JSON edit, restart, or fixed model list is needed. If a game is missed, use **Running apps → Add selected as game**, or **Add game…** to select its executable.

You do not need to start LM Studio's HTTP server manually. GamePause uses an already-running server's port, or temporarily opens the local server to capture loaded model settings, then returns it to its original state. Automatic-pausing, startup, exclusions, and optional connection/delay settings are controlled in the app.

Quit retains pending recovery for the next active run. Finish restoration before uninstalling. The installer and executables are currently unsigned; Windows may show a publisher warning. Download from this repository and compare the supplied SHA-256 checksums.

## Documentation

| Guide | Covers |
|---|---|
| [Installation](docs/INSTALLATION.md) | Installer, portable use, prerequisites, startup, upgrades, uninstall |
| [Usage and recovery](docs/USAGE.md) | Session behavior, manual controls, crash recovery |
| [Configuration](docs/CONFIGURATION.md) | Every setting, custom games, exclusions |
| [Troubleshooting](docs/TROUBLESHOOTING.md) | Diagnostics and common failures |
| [Validation](docs/VALIDATION.md) | Live Witcher 3 test, overhead, coverage limits |
| [Development](docs/DEVELOPMENT.md) | Build, architecture, safety, packaging |

## Performance and compatibility

On one Windows 11 / RTX 5090 PC with 16 logical CPUs, the installed v0.2.0 watcher used **14.1 MiB RAM** and **0.055% of total CPU capacity** during a 46-second Witcher 3 menu sample, including an inventory refresh. The dashboard was closed. This measures the watcher, not FPS or all machines. See [Validation](docs/VALIDATION.md).

This is a **0.2.0 preview**. Discovery is best effort. Protected processes and unconventional installations can require registering a game through the dashboard. Only the Steam / Witcher 3 lifecycle has been tested live. Other adapters have been checked against local installed metadata, not complete gameplay sessions.

Complete settings preservation currently requires LM Studio's internal WebSocket protocol alongside its CLI and native REST API. Protocol changes can require a GamePause update. Run `GamePauseCLI.exe --doctor` after upgrading LM Studio. If a recoverable snapshot cannot be captured, GamePause refuses to unload. It does not launch LM Studio or supervise clients that independently restart its server.

Active inference defers unloading until idle. Pause continuously running AI agents before gaming if you need VRAM released promptly.

## Build and contribute

Windows x64, the pinned Rust toolchain, and Visual Studio C++ Build Tools are required:

```powershell
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
.\target\release\GamePauseCLI.exe --doctor
```

See [Contributing](CONTRIBUTING.md) and [Security](SECURITY.md). Free under the [MIT license](LICENSE); binary distributions include dependency licenses. Independent community project, unaffiliated with LM Studio or launcher vendors.
