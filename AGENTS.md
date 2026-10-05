# GamePause coding guidance

Read CONTRIBUTING.md and docs/DEVELOPMENT.md, then the specification supplied for the current task. Historical release plans, reviews and the sections of DEVELOPMENT.md marked historical are reference, not authority to implement an old version or restore old assumptions.

## Architecture

- Preserve the existing Rust stack. The tray, menus, notifications and process/power integration use Win32 directly through windows-sys. The dashboard is Rust-native eframe/egui with Glow and AccessKit. Do not migrate to a webview, a web frontend or another GUI framework, and do not restore the pre-1.5.0 native-control dashboard, unless requested.
- app.rs coordinates the watcher lifecycle, control thread and discovery worker; detection_worker.rs owns the periodic process scanner. tray.rs, native_menu.rs and theme.rs own the Win32 tray UI; dashboard.rs and dashboard_theme.rs own the egui dashboard; game_icons.rs loads executable icons on its own thread, only while a dashboard window is open. config.rs owns validated settings and atomic persistence.
- engine.rs owns the session state machine and game/grace/override policy. coordinator.rs schedules checkpointed provider units; provider.rs and provider_runtime.rs define and route the provider contract. lmstudio.rs and lm_session.rs own LM Studio protocol integration and its units; the ollama_*.rs modules own the experimental Ollama adapter. recovery.rs owns journal storage; ownership.rs owns cross-process endpoint/service claims. Keep payloads adapter-owned.
- The dashboard runs on its own UI thread, consumes cloned shared state and sends tracked worker actions. It never creates provider adapters or performs model operations. Keep window handles on UI threads; no HWND crosses to engine workers. Provider calls stay on the control thread and never run on the detection thread.
- Never hold a UI RefCell borrow, bridge lock or shared mutex across a Win32 call that can dispatch messages, a native modal, renderer dispatch, process enumeration or provider/network work.
- Preserve bounded idle work, timeouts, output/response caps, and incremental discovery. A closed dashboard has no window or renderer and must not poll or redraw. Do not add telemetry or automatic runtime downloads.
- Ollama control is experimental, off by default and requires explicit opt-in. It is live-tested with Ollama 0.35.1 only (see docs/v1.0.0-ollama-review.md, Live evidence); do not claim other versions. A pause unloads every verified-local resident model; only local GGUF models advertising `completion` (not `embedding`) are restored, and other local models are unload-only and reported through the provider note. Remote or changed content refuses capture. Do not widen the restore subset or guarantees without evidence.
- New Ollama captures use the `frozen-remaining-v1` expiry policy: the keep-alive left at capture is replayed whole, and indefinite residency is replayed as indefinite. Journals carrying `absolute-observed-deadline-v1` must keep working under that policy. An unload acknowledgement never proves absence; only inventory does.
- Persist recovery intent atomically before model/server changes. Preserve the original snapshot through partial failures and clear recovery only after verification. Settings and the recovery envelope are version 3; the Ollama snapshot is version 1 and selects behavior by its `expiry_policy` string. Guard format changes, keep the guarded version-2 migrations and their backups working, and reject unknown formats rather than discard recovery.

## Verification

Use the pinned toolchain and lockfile. Run applicable focused checks followed by the project quality gates after substantial code changes:

```powershell
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Use mock backends and isolated fixtures by default. Do not contact live LM Studio or Ollama, load/unload models, run games, change startup registration, or run ignored interactive tests unless the task authorizes those checks. Do not run every ignored test as a batch; some are child-process fixtures invoked by ordinary tests, and others open windows on an interactive desktop. When live experiments are authorized, use a private data directory and observation mode first; never overwrite a real pending recovery journal. For live Ollama checks, disable the LM Studio provider in the test settings, trigger pauses with a harmless executable registered as a custom game, and leave model residency as you found it. Report live behavior not exercised.

Internet research is allowed for coding problems; the application's local-only design does not prohibit agent research. Prefer official protocol documentation and upstream SDK source. Keep private journals, source, prompts, paths, and credentials out of external queries/uploads.

Update relevant user/developer documentation with behavior changes. Do not commit, tag, push, or publish unless requested. Return changed files, acceptance results, exact check outcomes, and remaining limitations.
