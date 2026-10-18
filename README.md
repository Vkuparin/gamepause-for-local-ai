# GamePause for Local AI

[![CI](https://github.com/Vkuparin/gamepause-for-local-ai/actions/workflows/ci.yml/badge.svg)](https://github.com/Vkuparin/gamepause-for-local-ai/actions/workflows/ci.yml)
[![MIT](https://img.shields.io/badge/license-MIT-155e75.svg)](LICENSE)
[![Windows x64](https://img.shields.io/badge/platform-Windows_x64-155e75.svg)](docs/INSTALLATION.md)

**Give your games the VRAM. Get your local AI back afterward.**

GamePause is a small native Windows tray app. When a game starts, it saves the supported state of your local AI providers, pauses them, and restores them after you stop playing. LM Studio, Ollama and Hermes are enabled by default. Existing provider choices are preserved when upgrading. There is no model list to maintain.

![A real session: AI running, paused when a game starts, restored after it closes](docs/images/pause-cycle.gif)

**[Download the latest release](https://github.com/Vkuparin/gamepause-for-local-ai/releases/latest)** (installer or portable ZIP) · [Changelog](CHANGELOG.md) · [Installation guide](docs/INSTALLATION.md)

## What it does

- **Pauses AI when a game starts.** Games from Steam, Epic, GOG, Xbox, EA, Ubisoft Connect and Battle.net are found automatically. Add anything else from **Running apps** or with **Add game...**.
- **Restores it when you are done.** 30 seconds after the last game exits, your models come back. Starting another game cancels the countdown; alt-tabbing does not end the pause.
- **LM Studio, restored exactly.** Every loaded LLM and embedding model returns with the same identifier, variant, TTL and load configuration, and the result is verified. The local server is stopped while you play, so clients cannot load models back in, and returned to its original state afterward.
- **Ollama, when it is running.** Local models are unloaded. GGUF completion models are reloaded with their context and the keep-alive time they had left. If Ollama is not running, nothing happens and nothing is reported as an error.
- **Other AI servers.** Pick a program file, such as `llama-server.exe` or `koboldcpp.exe`, and GamePause stops it for gaming and starts it again with the same command line. Not yet tested with those tools themselves.
- **A rule per game.** **On** pauses automatically, **Off** ignores the game, **Ask** leaves AI running and offers a one-click pause, with **Always pause** and **Never pause** to remember the answer.
- **Manual control.** **Pause AI** and **Resume AI** in the dashboard and tray, plus an optional system-wide shortcut that works inside games.
- **Recovery that survives crashes.** What was unloaded is written to disk before anything changes. If GamePause or the PC goes down mid-pause, start GamePause again and it finishes the job.

It is local only: no telemetry, no downloads, no administrator service, and model control stays on localhost.

## Made for always-on AI assistants

GamePause started as a fix for one situation: an assistant such as [Hermes Agent](https://hermes-agent.nousresearch.com) or [OpenClaw](https://openclaw.ai) keeps a large local model loaded around the clock, and then you want to play something. GamePause saves and unloads models used by these clients through LM Studio or Ollama.

LM Studio's HTTP server is stopped during gaming by default, so requests fail until it is restored. Ollama normally stays running, allowing clients to reload models. Under **Advanced > Ollama**, enable **Prevent Ollama from reloading models while gaming** to temporarily close and restart a verified Windows Ollama Desktop 0.40.3 installation, including its UI. Requests arriving during shutdown may be canceled. Unsupported installations keep ordinary unloading; external restarts are left alone. [Details](docs/USAGE.md#background-ai-applications).

## One window, four states

The status card always shows one of four color-coded states: **AI running** (blue), **AI paused** (orange), **Loading** (yellow) and **AI needs attention** (red). Below it are one line for each AI app you have installed or running, the game that triggered the pause, and roughly how much memory was freed.

| Dark | Light |
|---|---|
| ![GamePause for Local AI dashboard, dark theme: LM Studio and Ollama paused while a game runs](docs/images/dashboard-dark.png) | ![GamePause for Local AI dashboard, light theme: AI running and watching for games](docs/images/dashboard-light.png) |

Light, dark and Windows high-contrast themes are supported, and the dashboard follows your Windows setting by default. **Advanced** holds the connection settings, detection options, the shortcut, read-only diagnostics and a round-trip test that pauses and restores your live models on request.

## Quick controls in the tray

Closing the window leaves GamePause watching from the tray. Left-click the icon to open the dashboard, or right-click for the current state and the quick controls.

![GamePause tray menu](docs/images/tray-menu.png)

## Get started

1. Download **Setup.exe** from [Releases](https://github.com/Vkuparin/gamepause-for-local-ai/releases) and install it. GamePause starts in the tray and, by default, at Windows sign-in.
2. Use LM Studio, Ollama or Hermes as usual. For LM Studio, its `lms` command-line tool must be installed; GamePause finds it on its own. For Hermes, keep its Desktop/backend open; check **Advanced > Hermes** if upgrading from 2.6.0 with Hermes disabled. A Hermes-only setup needs no LM Studio installation.
3. Play. Automatic pausing is on from the first launch.

If a game is not recognized, open **Running apps** while it runs and choose **Add as game**. GamePause also points out fullscreen programs that look like games it does not know.

Prefer a package manager? With [Scoop](https://scoop.sh):

```powershell
scoop bucket add gamepause https://github.com/Vkuparin/gamepause-for-local-ai
scoop install gamepause
```

The installer and executables are unsigned, so Windows may show a publisher warning. Download from this repository and compare the SHA-256 checksums published with each release.

## Good to know

- **Busy models wait.** If a model is generating when a game starts, unloading waits until it finishes. Stop long-running agents before playing if you need the memory right away.
- **Ollama restores a subset.** Embedding models and models with unknown format are unloaded but not reloaded; load options, parallelism and conversations are not preserved. [Details](docs/USAGE.md#background-ai-applications).
- **Hermes pauses its managed runtime.** Native control leaves Hermes open. When the messaging gateway owns the runtime, GamePause closes and reopens that gateway as well. It restores captured model content, live context and supported load fields. Conversation/KV state and idle timing are not preserved. A live pause/restore passed with Qwen3.8-27B-UD-Q4_K_M on CUDA llama.cpp b11370. [Details](docs/USAGE.md#hermes-local-models).
- **LM Studio updates can need a GamePause update.** Full settings restoration uses LM Studio's internal protocol. After upgrading LM Studio, check **Advanced > Diagnostics**. If a complete snapshot cannot be captured, GamePause refuses to unload.
- **Quitting keeps a pending restore.** Models paused when you quit come back the next time GamePause runs. Restore before uninstalling.
- **Detection is best effort.** Protected processes and unusual installations may need to be added by hand.

## Tested with

| AI app | Version | Notes |
|---|---|---|
| LM Studio | 0.4.25 | Full restore uses LM Studio's internal protocol, so a newer version can need a GamePause update. |
| Ollama | 0.35.1, 0.40.0, 0.40.3 | One local GGUF model per run. Desktop shutdown/restart tested with 0.40.3 only. Embedding, vision and multi-model sessions have automated coverage only. |
| llama.cpp, KoboldCpp | none yet | Stop and restart was tested with stand-in programs. |
| Hermes managed runtime | llama.cpp b11370, CUDA | Native pause/start and Qwen3.8-27B-UD-Q4_K_M replay passed; live context and saved preferences matched. No broader Hermes release range claimed. |

Using another version? **Advanced > Diagnostics** shows what GamePause can see, and a [report](https://github.com/Vkuparin/gamepause-for-local-ai/issues) with that output helps extend this table.

## Documentation

| Guide | Covers |
|---|---|
| [Installation](docs/INSTALLATION.md) | Installer, portable use, prerequisites, startup, upgrades, uninstall |
| [Usage and recovery](docs/USAGE.md) | Session behavior, manual controls, crash recovery, CLI |
| [Configuration](docs/CONFIGURATION.md) | Every setting, custom games, exclusions |
| [Troubleshooting](docs/TROUBLESHOOTING.md) | Diagnostics and common failures |
| [Validation](docs/VALIDATION.md) | What was tested live, measured overhead, coverage limits |
| [Development](docs/DEVELOPMENT.md) | Build, architecture, safety rules, packaging |

## Build and contribute

Windows x64, the pinned Rust toolchain and Visual Studio C++ Build Tools are required:

```powershell
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

See [Contributing](CONTRIBUTING.md), [Security](SECURITY.md) and the [roadmap](docs/ROADMAP.md). Free under the [MIT license](LICENSE); binary distributions include dependency licenses. GamePause is an independent community project, not affiliated with LM Studio, Ollama, Hermes or any launcher vendor.
