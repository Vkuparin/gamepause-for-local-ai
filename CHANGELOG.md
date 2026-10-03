# Changelog

## 0.3.0 — 2026-10-04 (preview)

### Resilience
- Degrade instead of dying when the data directory is temporarily unavailable; recovery state is preserved and control resumes when the path is writable again.
- Contain a panicking game-discovery adapter behind the last-good inventory, so a single bad launcher can no longer take down detection.
- Log uncaught panics to the local `gamepause.log` (best-effort, local-only) and surface an explicit "Restore failed — AI not restored, click Restore" status while keeping the recovery journal intact and resumable.

### UX and native polish
- Per-Monitor v2 DPI with `WM_DPICHANGED` relayout, so the dashboard stays crisp and unstretched across mixed-DPI multi-monitor setups.
- Follow the Windows theme: DWM dark caption for the window and tray-anchored panel, and control colors taken from system brushes so light and dark both look native.
- **Status dots in the games list** — a colored dot per row: green = running, amber = automatic pausing off, gray = idle — so the per-game state reads at a glance as well as in the row text.
- Group-box sectioning (Games / Settings / Actions) and a tighter, single-source-of-truth layout grid.
- State toasts plus an optional system sound on pause, restore, and failure.
- Tray icon now reflects state — idle, paused, and attention.
- Usage-path fixes and expanded acceptance tests.

### Development and scripting
- **Test round-trip (verify AI):** run the full capture → unload → confirm-server-emptied → restore → field-compare cycle on demand from the dashboard, the tray, or `GamePauseCLI.exe --verify`, and see which step failed.
- **Diagnostics core:** a structured doctor report (LM Studio version, server state, loaded models, data-dir writability) backing both the `--doctor` panel and the CLI.
- **Stable CLI output for scripting:** `--status` (key=value state) and `--games` (name / launcher / path), plus `--doctor` and `--verify`; the read-only commands work while a GUI instance is running.
- **Per-capture/restore WS logging:** each WebSocket step records the LM Studio version and a `success` or `failed:<field>` line (naming the diverged field) to the local log.

### Documentation
- README "Recovery" section: what to do if GamePause disappears mid-pause — relaunch it and the restore resumes.

### Deferred to a follow-up
- CI for the interactive popup-reentrancy test (Windows runner only); kept as an ignored local test for now.

## 0.2.0 — 2026-10-03 (preview)

- Enable automatic pausing by default and migrate the old observation/active setup.
- Add an on-demand native dashboard with searchable games, running-app registration, executable browsing, ignore/enable controls, and live settings.
- Open the existing dashboard when launched again; keep sign-in startup quiet in the tray.
- Discover new games automatically, request early Steam metadata refresh for unfamiliar processes, and remove broad library-folder classification.
- Exclude known background utilities from the game inventory.
- Stay running when the LM Studio CLI is unavailable and retry control automatically.
- Handle an initially stopped HTTP server during capture and preserve its original state through recovery.
- Preserve pending recovery when automatic pausing is switched off.
- Remember session game paths through exclusions/removal and wait for initial discovery before recovering after a restart.
- Exclude REDlauncher prelaunch helpers and clear abandoned capture errors when a session ends without unloading.
- Expand regression tests and document the normal installed workflow.

## 0.1.1 — 2026-10-03 (preview)

- Fix a crash when the tray menu remains open through a status timer tick. Windows dispatches timer messages inside the popup menu; the UI now releases state borrows before native calls.
- Prevent nested tray clicks from opening a second menu.
- Add an automatic timer/menu regression test and a separate interactive Windows test for three popup openings, each spanning two timer callbacks.
- Upgrading preserves configuration, recovery state, and startup registration.

## 0.1.0 — 2026-10-03 (preview)

- First public release, implemented in native Rust for Windows x64.
- Steam, Epic, EA, Ubisoft Connect, Battle.net, Xbox, and custom installation discovery.
- Native process polling, executable path caching, and configurable exclusions.
- Dynamic capture and verified restoration of LLM and embedding instances with complete load settings.
- Server shutdown during gaming, exit grace period, multi-game sessions, retry backoff, durable recovery, and instance locking.
- Native tray controls, console diagnostics, startup at Windows sign-in, per-user installer, portable ZIP, checksums, and dependency licenses.
- Live Steam / Witcher 3 validation and documented resource measurements.

This preview uses LM Studio's internal control protocol. See the validation and troubleshooting guides for compatibility and recovery limits.
