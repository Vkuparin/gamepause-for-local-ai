# v1.5.0 UI design and implementation journal

## Scope and authority

The current task is the attached full UI redesign specification and mockup, with the user's explicit v1.5.0 version and testing instructions. Historical release plans do not define this work. Keep existing behavior, recovery formats, worker commands and settings compatibility. The owner authorized committing and publishing on 2026-10-05 after confirming the app works.

## Design decision

Following the user's explicit toolkit steering, replace the dashboard with eframe/egui 0.33.3, using its native Glow renderer, AccessKit and egui_extras tables. Keep the native tray and existing worker routing. The dashboard owns its event loop on a dedicated UI thread; no window handles travel to engine workers. Use charcoal surfaces, restrained borders, orange actions/selection, green verified success, and distinct error states. Preserve Light/System and high-contrast choices. Rust dependencies are authorized; no webview or frontend is used.

The main shell uses a status hero, compact automation strip, Games/Running apps/Ignored navigation, aligned table columns, a selected-entry card and a quiet version/Activity/Quit footer. Advanced settings use the same surfaces and controls. Activity holds detailed state/diagnostic text and can read the current worker log on request with a 64 KiB cap. Successful command feedback expires; actionable errors remain visible.

## Status

Implementation and final automated/rendered verification are complete. The owner approved v1.5.0 publication. The release branch, tag and verified downloads are published. The live/physical acceptance limits below remain open.

- Read CONTRIBUTING.md, docs/DEVELOPMENT.md and the supplied specification.
- Inspected dashboard, theme, shared presentation/control policy and confirmation flows.
- Baseline `cargo build --locked`: passed with the pinned toolchain.
- Implemented the eframe dashboard, shared theme/vector icons, status hero, game tables, selected-entry card, grouped Advanced settings, Activity history and expiring success feedback.
- Added themed Add/Rename/Remove/Resume/Verify/Ollama dialogs. Tray gameplay Resume and round-trip verification route to the same dashboard dialogs. The worker still validates requests and transient gameplay offer IDs.
- Preserved pure save/rename/add/verification-report regression tests. Replaced obsolete Win32-control geometry/GDI fixtures with egui page/state/DPI rendering tests. Removed the unused Rich Edit layer and native dashboard drawing code; native tray palette/menu code remains.
- Focused `cargo test --locked dashboard:: -- --test-threads=1`: 13 passed, 2 isolated interactive fixtures ignored. Clippy is clean.
- Ran `ui_design_review_snapshots` individually with `--ignored`; it opens a fictional-state renderer window, captures its own framebuffer and exits. It does not create a watcher, contact providers or write settings/recovery. Fixed the card sizing, selected-row clipping and table resize issues found in the first review.
- Existing untracked AGENTS.md and playtest_notes.md belong to the user and are preserved.

## Verification plan

Use focused unit tests while editing. After implementation, run `cargo fmt --check`, `cargo test --locked` once, `cargo clippy --locked --all-targets -- -D warnings`, and build both executables. Use isolated native fixtures for UI review, with fake shared state and no provider backend. Do not contact live providers, change startup registration or touch a real recovery journal. Record visual/DPI acceptance and any limits here before delivery.


## Lifecycle correction and acceptance progress

The isolated lifecycle fixture exposed missed repaint wakeups for a hidden eframe window. Close now destroys the window/renderer and retains a mailbox-waiting UI thread. Reopen reuses eframe's event loop on that same thread. Stop wakes either the open renderer or the closed mailbox and joins outside all shared locks. `ui_bridge_hides_reopens_and_stops_without_backend` now passes (its historical test name is retained). No renderer remains allocated while closed.

Current focused dashboard checks: 13 passed, 2 explicit interactive fixtures ignored in the default run. The isolated visual fixture exported Games, compact/large windows, Running apps, Ignored, all six settings categories, gameplay warning, Add game, Activity and Light/error views. Reviewed the actual renderer screenshots and fixed card width, table spacing/column persistence, selection clipping and primary/detail action placement. No artwork networking was added. [The final Games preview](images/v1.5.0-dashboard.png) contains fictional fixture data, not a live provider session.

## Final verification

- `cargo fmt --check`: passed.
- `cargo test --locked`: final verification passed after implementation and the close/theme correction. Library: 257 passed, 6 ignored; CLI integration: 10 passed; binary/doc tests: no failures.
- `cargo clippy --locked --all-targets -- -D warnings`: passed with no warnings.
- `ui_design_review_snapshots`: passed when invoked individually with `--ignored --test-threads=1 --nocapture`.
- `ui_bridge_hides_reopens_and_stops_without_backend`: passed when invoked individually with the same flags, after fixing close/reopen.
- `cargo build --locked --release`: passed for the final source, producing both executables. Windows ProductVersion is 1.5.0 for each; each executable is 7,374,848 bytes. Theme-change messages do not reopen a closed dashboard.

## Changed files

- `src/dashboard.rs`: eframe UI, worker routing, table/status models, dialogs, Activity and focused/visual/lifecycle tests.
- `src/dashboard_theme.rs`: centralized visual tokens, fonts, vector icons and checkboxes.
- `src/tray.rs`, `src/restore_dialog.rs`, `src/theme.rs`, `src/lib.rs`: UI bridge, shared confirmation routing and removal of obsolete dashboard control painting.
- Removed `src/rich_text.rs`, which served only the replaced native details control.
- `Cargo.toml`, `Cargo.lock`, `installer/GamePause.iss`: v1.5.0 metadata and locked Rust-native dependencies; image encoding is a development-only screenshot dependency.
- `README.md`, `CHANGELOG.md`, `docs/USAGE.md`, `docs/DEVELOPMENT.md`, this journal and `docs/images/v1.5.0-dashboard.png`: current UI behavior and review evidence.
- `.gitignore`: exclude isolated scratch fixtures and renderer captures.

## Remaining acceptance limits

Live LM Studio/Ollama unload/reload, actual games, sleep/wake, startup registration, installer behavior and physical multi-monitor/screen-reader acceptance were not exercised. Existing backend/mock tests passed. No real settings or recovery journal was changed, and no commit, tag, push or publication was made. Generic icons deliberately replace unavailable game artwork. Current model/server residency remains unpolled after verified operations. Compact layouts scroll and hide only the secondary platform column; the physical DPI review still belongs to human acceptance.

## Publication preparation - 2026-10-05

The owner confirmed the redesigned app works and requested "commit and publish". Updated release/acceptance documentation and prepared the v1.5.0 installer and portable ZIP. The package verifier found missing license texts in several new crate archives. Added pinned upstream license copies and bundled font licenses, and updated packaging to include them and the dashboard preview. Package checks now pass for checksums, executable versions, defaults, license texts and archive contents. Publication will include the installer, ZIP, SHA256SUMS.txt, BUILD-INFO.json and a separate acceptance record. Existing local AGENTS.md and playtest_notes.md remain excluded from the commit. No new live/physical acceptance is inferred from the owner's feedback.

## Publication completed

Published [GamePause 1.5.0](https://github.com/Vkuparin/gamepause-lmstudio/releases/tag/v1.5.0) from commit `4b127db13879a890bf8a47f51feb351cd586470f` on `codex/v1.5.0-release`, with annotated tag `v1.5.0`. The installer, portable ZIP, SHA256SUMS.txt, BUILD-INFO.json and RELEASE-ACCEPTANCE.md are uploaded; all five GitHub asset digests match local SHA-256 hashes. GitHub CI and release-build workflows were started and remain in progress at publication time. Local final checks and package verification passed.

Automatic approval review rejected the combined push because it included the default `main` branch. Published the authorized release branch and tag instead; `main` remains unchanged. This journal update is a separate documentation commit after the immutable release tag. Owner-local AGENTS.md and playtest_notes.md remain untracked and untouched.