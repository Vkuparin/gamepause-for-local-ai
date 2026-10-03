# GamePause for LM Studio

[![CI](https://github.com/Vkuparin/gamepause-lmstudio/actions/workflows/ci.yml/badge.svg)](https://github.com/Vkuparin/gamepause-lmstudio/actions/workflows/ci.yml)
[![MIT](https://img.shields.io/badge/license-MIT-155e75.svg)](LICENSE)
[![Windows x64](https://img.shields.io/badge/platform-Windows_x64-155e75.svg)](docs/INSTALLATION.md)

**Give your games room. Bring your local AI back afterward.**

GamePause is a small native Rust tray app for Windows. It notices when a game starts, saves the models currently loaded in LM Studio, unloads them, and restores them afterward. The model name can change: there is no fixed model list to maintain.

## Features

- Discovers Steam, Epic, Xbox, EA, Ubisoft Connect, and Battle.net installations. Local inventories refresh every 30 seconds. Xbox package discovery refreshes every five minutes and defers during gaming.
- Checks running processes every two seconds using Windows APIs and cached executable paths. Desktop shortcuts work too.
- Captures loaded LLM and embedding instances, exact model selections, identifiers, TTL policies, and complete load configuration. Restoration verifies the settings.
- Stops LM Studio's HTTP server during gaming by default to prevent HTTP clients from immediately loading models again.
- Waits 30 seconds after the last game exits. Starting another game cancels the delay; alt-tabbing keeps AI paused.
- Keeps a durable recovery journal through partial failures and restarts.
- Includes native tray controls, automatic startup at Windows sign-in, a per-user installer, and a portable ZIP.

No Python runtime, administrator service, telemetry, recursive game scans, or permanent model-name configuration. Model control uses localhost.

## Get started

1. Download the **Setup.exe** from [Releases](https://github.com/Vkuparin/gamepause-lmstudio/releases). Run the installer. Startup at sign-in is checked by default.
2. Keep LM Studio open, start its local server on port **1234**, and load the models you want restored. Its `lms` command must be installed; see [Installation](docs/INSTALLATION.md).
3. Launch GamePause. First run uses **observation mode**, which detects games without changing AI state. Launch a game and hover over its tray icon to check detection.
4. Right-click the tray icon, choose **Open configuration**, and change `"mode": "observe"` to `"mode": "active"`. Save, quit GamePause, and reopen it.
5. Launch games normally. Use the tray menu for manual pause, restore, detection, logs, configuration, and startup settings.

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

On one Windows 11 / RTX 5090 PC with 16 logical CPUs, the native watcher used **15.7 MiB RAM** and **0.076% of total CPU capacity** during a 45-second Witcher 3 menu sample, including an inventory refresh. This measures the watcher, not FPS or all machines. See [Validation](docs/VALIDATION.md).

This is a **0.1.0 preview**. Discovery is best effort. Protected processes, unconventional installations, and unusual helpers may require configuration. Only the Steam / Witcher 3 lifecycle has been tested live. Other adapters have been checked against local installed metadata, not complete gameplay sessions.

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
