# Development

## Build and checks

Windows x64, Rust via rustup, Visual Studio C++ Build Tools, and the Windows SDK are required. The toolchain and dependency lockfile are committed.

```powershell
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
.\target\release\GamePauseCLI.exe --observe --headless --duration 10 --data-dir .\scratch
```

Default behavior automatically pauses AI. For non-mutating diagnostics, explicitly pass `--observe`. Use a private data directory for development. Do not overwrite a real pending journal, and do not commit runtime files.

## Architecture

| Module | Responsibility |
|---|---|
| `app.rs` | CLI, watcher lifecycle, discovery worker, shared tray status, diagnostics, bounded logging |
| `tray.rs` | Win32 hidden window, notification icon/menu, dashboard routing, sign-in registration |
| `dashboard.rs` | On-demand native controls, game search/registration, exclusions, live settings |
| `processes.rs` | Toolhelp process snapshots, creation-time/path cache, folder matching and exclusions |
| `discovery.rs` | Launcher metadata adapters, incremental inventory, safe parser boundaries |
| `lmstudio.rs` | Bounded CLI commands, local REST, WebSocket capture/load, restoration verification |
| `engine.rs` | Session state machine, journal, grace period, retries and interruption |
| `config.rs` | Validated settings, atomic writes, exclusive data-directory lock |

The watcher polls native process metadata every two seconds. It only queries executable paths for new `(PID, creation time)` pairs. Inventory refresh runs on a separate worker and retains the previous successful adapter inventory after errors. No installed-game directory tree is recursively scanned. Xbox package discovery uses a bounded occasional PowerShell query and is deferred during gaming.

The UI uses Win32 directly: no webview, GUI framework, or async runtime. Completed gaming pauses do not repeatedly contact LM Studio or rewrite the journal. Runtime status is written only when it changes, with bounded log rotation. The dashboard is created on demand and destroyed when closed; running-app rows are produced only on request. Settings travel through the worker action channel, validate and save atomically before replacing scanner/backend configuration. Turning off automatic pausing still allows recovery after recognized games exit. The old broad Steam-library fallback is replaced by a one-shot refresh request for new unfamiliar processes inside known libraries.

## LM Studio integration

The CLI supplies model/server inventories and unload/start/stop operations. Native `/api/v1/models` supplies additional settings. The internal `/llm` and `/embedding` WebSocket interfaces supply raw load configuration and load models using an API override layer. Capture can temporarily start the server when it was initially stopped, and always attempts to stop that temporary server even if capture fails. Recovery uses the recorded server port and original running state. This preserves settings outside the public SDK's simplified configuration surface.

The protocol is an internal dependency, not a stability promise. Relevant primary references: [LM Studio CLI](https://lmstudio.ai/docs/cli), [native REST API](https://lmstudio.ai/docs/developer/rest), [LM Studio Python SDK source](https://github.com/lmstudio-ai/lmstudio-python), and [Windows Toolhelp process snapshots](https://learn.microsoft.com/en-us/windows/win32/toolhelp/taking-a-snapshot-and-viewing-processes). Check protocol changes against the official SDK source and run a full live cycle before shipping.

CLI inventories may report a base `modelKey` plus `selectedVariant` before loading, then a variant `modelKey` after loading by variant. Identity validation accepts equivalent representations while refusing a different quantization/file. The loader checks the saved variant remains selected and uses the catalog base key so instances remain visible through native REST. Raw key/value fields are compared independent of ordering; native exposed fields are also checked. Changed variant selection retains recovery and requires resolving the catalog selection.

## Recovery rules

1. Capture every loaded instance and complete available settings before unloading.
2. Write intent atomically and flush before stopping the server, unloading, or loading.
3. Preserve the original snapshot across partial errors. Retry with backoff.
4. Recheck game processes before starting recovery and between model loads. A new game interrupts restoration; the next tick returns to pause.
5. Clear recovery only after all captured instances and the original server state are verified.

Observation mode does not execute pending recovery. Unknown journal formats are rejected. The exclusive Windows file handle prevents concurrent mutation through one data directory. Grace and retry timing use monotonic elapsed time.

Recovery waits for the first discovery result. The journal also remembers games seen during its session; removing or ignoring an entry cannot make an already-running game disappear from recovery checks. Old journals without these paths remain readable. Manual restoration uses the same discovery gate and checks recognized games even when user exclusions disable new pausing.

## Packaging

Install Inno Setup 6 (tested with 6.7.3). Run:

```powershell
.\scripts\build_release.ps1 -Iscc 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
```

This builds both native executables, copies docs and dependency licenses, packages a portable ZIP, compiles the per-user installer, and writes `dist\SHA256SUMS.txt`. Release optimizations use size optimization, LTO, stripping, and one codegen unit. The Windows target config statically links the C runtime, avoiding a separate Visual C++ redistributable. Destination PCs need no language runtime. See the [Rust linkage reference](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes).

CI runs Windows tests, formatting, and Clippy. The tag-triggered release workflow builds distribution artifacts; publication is a separate action. Before publishing, inspect tracked files for private data, perform the live validation checklist, test install/uninstall/startup registration, verify checksums, and publish release notes with compatibility limits.

## Interactive tray regression

The default unit suite covers timer reentry while a menu session is alive. On an interactive Windows desktop, also run:

```powershell
cargo test --locked native_popup_survives_repeated_timer_reentry -- --ignored --test-threads=1 --nocapture
```

This test opens and dismisses three native menus belonging to its own observation-only test process. Each must survive at least two actual status timer callbacks. It does not load or unload models, change startup settings, or send actions to the running watcher. Keep the desktop unlocked and avoid interacting with the test menus while it runs. It is excluded from unattended CI because popup tracking requires an interactive desktop.

Win32 popup tracking runs a nested message loop. Never hold a UI-state RefCell borrow or shared-state mutex across native calls that may dispatch messages synchronously.
