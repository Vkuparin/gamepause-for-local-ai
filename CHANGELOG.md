# Changelog

## 1.6.0 — 2026-10-06

- Restyle the dashboard after the three state mockups: blue **AI RUNNING**, orange **AI PAUSED**, yellow **LOADING** and red **AI NEEDS ATTENTION**, with a glowing state ring, a state-colored main button, attached tabs, a tinted title bar (Windows 11) and a state-colored window icon. Light and high-contrast modes are kept.
- Shorten status text. Every worker state folds into one of the four looks; provider lines read **Ready**, **Pausing AI**, **Models unloaded successfully**, **Resuming AI** or **Models restored**. Idle model residency is still not polled.
- Show each game's own executable icon, loaded only while the dashboard is open through a bounded lookup, and GamePause's own launcher marks. No artwork is downloaded and no vendor logos are bundled.
- Remove the Status column, make table columns sortable, and move Refresh to Advanced > Detection (it stays in the tray menu).
- The installer now starts GamePause in the system tray instead of opening the dashboard, and its texts say so.
- The owner requested publication. No owner test report for this build was supplied, and no new live provider, installer or physical accessibility coverage is claimed. See [Acceptance](docs/ACCEPTANCE.md).

## 1.5.0 — 2026-10-05

- Replace the dashboard with Rust-native eframe/egui, matching the charcoal/orange mockup with a concise status hero, searchable game/application tables and selected-entry cards.
- Group Advanced settings and use themed Add, Rename, Remove, gameplay Resume, live verification and experimental Ollama dialogs. Preserve worker commands, saved settings and recovery safety checks.
- Add bounded Activity history, detailed status/diagnostics and five-second success feedback. Retain actionable failures. Use shared vector icons, Windows/bundled fonts, responsive layouts and DPI scaling.
- Keep the native tray and CLI. Closing the dashboard releases its renderer; Quit retains pending recovery. The owner confirmed the app works and approved publication; no new live provider compatibility or physical accessibility coverage is claimed.

## 1.0.0 — 2026-10-05

- Add state accents, colored game pausing words, feedback below help and a version footer. Add a saved Appearance choice for the dashboard and tray menu. Fix dark button hover, Advanced scrolling and stale group backgrounds after switching to Light; clarify the gameplay Resume warning.
- Fix dashboard group boxes covering buttons, checks and tabs. Hide window/text scrollbars when content fits and remove frames around read-only summaries. Consolidate the tray/dashboard action as Resume AI, which releases the manual hold and resumes saved AI immediately; gameplay still requires confirmation.
- Show detected games, enabled-provider outcomes, reasons and next steps in the core dashboard and tray. Share guarded Pause/Resume availability and persistent command feedback.
- Confirm immediate restoration during gaming for the listed live process instances, with separate optional Ignore choices. New games, relaunches, unknown detection and app restart revoke that approval.
- Persist default-off Advanced visibility, independent notification/sound choices and selected custom-game removal. Use one sound source and coalesce delayed success notifications.
- Add resizable native layouts, measured DPI captions, keyboard help, tooltips and dark dashboard drawing with system-color high-contrast fallback. Preserve native control classes and input handling.
- Migrate settings and recovery to guarded version-3 formats with byte-preserving older-file backups. Retain original LM settings/server state, independent provider recovery and atomic intent before mutations.
- Add off-by-default experimental Ollama control for supported local GGUF completion models, verified identity/context and remaining finite observed residency deadlines. No live compatibility is claimed; unsupported capabilities/options remain refused.
- Keep periodic detection independent of blocked control, refuse competing provider ownership, add read-only Advanced diagnostics and preserve existing CLI fields with provider-qualified evidence.
- Reconcile power events with fresh discovery/process evidence and a new full recovery delay. Preserve manual holds and original recovery data.

The owner reported human tests passed and approved publication on 2026-10-05. The reviewed installer and ZIP are published unchanged. [Acceptance](docs/ACCEPTANCE.md) records their hashes and evidence limits. Further UI and UX improvements are planned for later versions.

## 0.3.5 — 2026-10-04 (stabilisation preview)

- Make release discovery panics recoverable with unwinding; bound launcher metadata/nesting and contain native callback panics. Add a production-profile panic regression.
- Journal round-trip verification before unloading, recover other models after partial failures, retain unresolved recovery, and preserve initially stopped/nondefault-port server state. Validate snapshots before mutation.
- Guard verification against games, observation/manual pause, discovery errors, and pending recovery; confirm disruptive GUI tests and reject duplicate requests. CLI restore uses remembered games even after removal/exclusion.
- Refuse CLI verify while the GUI owns the lock, reject conflicting commands, distinguish live status from stale files, and escape line delimiters/backslashes. Games remain a cached inventory.
- Return independent doctor probes even when CLI/configuration/data-directory checks fail; doctor never changes model/server state. WS compatibility is unknown if it cannot be checked safely.
- Log actual configuration/load/restore checks from normal and diagnostic paths, with instance identifiers and an accurately labelled cached CLI version. Bound WS operations and subprocess pipe completion; drain and report oversized output.
- Notify pause/restore only on completion, keep pending recovery visible during countdown, scale fonts/list rows/drawing offsets across DPI, and cache fonts until child windows are destroyed.
- Use the Windows app-theme preference for caption colour, follow theme changes, and respect high contrast. Full dark client controls and an embedded doctor view are explicitly deferred. System notification sounds are currently always enabled.
- Refresh preview/docs/installer version and omit internal review/plan files from release documentation bundles. Schema-2 recovery and existing settings remain compatible.

### Validation limits
- Local automated, CLI process, release panic, and interactive popup/DPI checks are recorded in `docs/VALIDATION.md`. Live v0.3.5 LM Studio/gameplay and installer upgrade validation are still required before publication. Historical v0.2.0 performance measurements have not been relabelled.

## 0.3.0 — 2026-10-04 (preview)

### Resilience
- Degrade instead of dying when the data directory is temporarily unavailable; recovery state is preserved and control resumes when the path is writable again.
- Contain a panicking game-discovery adapter behind the last-good inventory, so a single bad launcher can no longer take down detection.
- Log uncaught panics to the local `gamepause.log` (best-effort, local-only) and surface an explicit "Restore failed — AI not restored, click Restore" status while keeping the recovery journal intact and resumable.

### UX and native polish
- Per-Monitor v2 DPI with `WM_DPICHANGED` relayout, so the dashboard stays crisp and unstretched across mixed-DPI multi-monitor setups.
- Follow the Windows theme: DWM dark caption for the window and tray-anchored panel, and control colors taken from classic system brushes; full dark client rendering remained incomplete.
- **Status dots in the games list** — a colored dot per row: green = running, amber = automatic pausing off, gray = idle — so the per-game state reads at a glance as well as in the row text.
- Group-box sectioning (Games / Settings / Actions) and a tighter, single-source-of-truth layout grid.
- State toasts plus a system sound on pause, restore, and failure.
- Tray icon now reflects state — idle, paused, and attention.
- Usage-path fixes and expanded acceptance tests.

### Development and scripting
- **Test round-trip (verify AI):** run the full capture → unload → confirm-server-emptied → restore → field-compare cycle on demand from the dashboard, the tray, or `GamePauseCLI.exe --verify`, and see which step failed.
- **Diagnostics core:** a structured doctor report (LM Studio version, server state, loaded models, data-dir writability) backing CLI output; an embedded dashboard diagnostics view remained incomplete.
- **Stable CLI output for scripting:** `--status` (key=value state) and `--games` (name / launcher / path), plus `--doctor` and `--verify`; the read-only commands work while a GUI instance is running.
- **Per-capture/restore WS logging:** CLI verification stage logs record the CLI version (actual WS instrumentation was completed in 0.3.5) and a `success` or `failed:<field>` line (naming the diverged field) to the local log.

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
