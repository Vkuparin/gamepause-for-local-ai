# Validation

## Owner acceptance - 2026-10-05

The owner reported "Human tests passed" and explicitly authorized pushing and publishing v1.0.0. This approval applies to the final appearance review build below. The installer and ZIP are published unchanged. Later versions will focus on further UI and UX improvements.

- Installer SHA-256: `2fe1061702516eec3227d408ea988ca57e934cd04176ee0c705122e0fcd0f1b6`.
- ZIP SHA-256: `8fc85ed1e29e5ccf2851f8903760cf5416d784cb1f265bec0b67ff36f35eae0f`.
- Source fingerprint: `59cf957fde1eae66594731c5ff02dfff7100bc1f2ce094e213a4a08ba036c3a9`.

The owner did not provide a scenario-by-scenario report or environment details. Do not infer specific provider versions, model identities, accessibility, power, startup or installer-upgrade coverage from this approval. Ollama remains experimental and off by default, with no live compatibility range claimed. Earlier pending-review entries below record the state before this approval and do not override it. Bundled candidate documentation and BUILD-INFO describe the build-time review status; this record and the release acceptance asset record the subsequent approval.


## State accents and appearance feedback - 2026-10-05

The dashboard now uses a blue accent for watching/coexistence, orange for confirmed pause/manual hold/countdown, and mellow yellow for transitions or uncertain evidence. State text remains authoritative; watching does not assert that idle models are loaded. Only the metadata words on/off receive blue/orange in list and selection details. Light mode uses darker text shades. Feedback is below help; the version is in the footer.

Advanced Appearance offers Follow Windows (default), Light and Dark. Existing settings default to Windows. Its dedicated atomic save updates only appearance after persistence; failed saves and hidden Advanced requests retain the prior choice. Provider configuration, transports, scanner state, manual hold and recovery are left intact. High contrast takes precedence. Native tray menus retain labels, IDs, checks, disabled flags and keyboard handling while using the saved palette and state accent.

Push buttons in both palettes use owner drawing rather than mixed theme-animation drawing. Group boxes fill their uncovered background in both palettes. Child movement is deferred, with a complete fallback after failed batches, and one final redraw; the composited parent clips children. Scroll-only movement does not toggle text scrollbars. Selection details use the built-in Windows Rich Edit control with selection/scroll preservation and metadata-only colors. The gameplay warning and optional Ignore help use plain English and fit measured 96/144/192 DPI fonts.

Validation: 82 focused tests passed (dashboard 23, theme three, tray 12, application 27, config ten, native menu one, Rich Edit two, restore dialog four). Three individually selected interactive fixtures passed: rendered dashboard/scrolling, DPI/GDI relayout and real popup/timer reentry alternating appearance. The rendered fixture exercises 30 Advanced down/up cycles, hover/pressed states, appearance dispatch awaiting persistence, mode switching, list/selection and bounds. Final measured GDI counts were 101/101 after 98 warmups and 49 relayouts. Light/dark captures were inspected; the stale dark group backgrounds are corrected. Formatting, Clippy/all targets and diff checks passed. The full suite was not repeated for this feedback ticket.

Human appearance/accessibility and live recovery acceptance remain pending. No installed application, real journal, provider/model/server, startup registration or game was changed. This build remains unpublished and requires further human feedback before release.

## UI feedback correction � 2026-10-05

The human screenshot exposed group boxes above overlapping sibling controls. The corrected dashboard moves group boxes to the bottom of the sibling order, keeps native controls and dark painting, removes frames from read-only summaries and shows scrollbars only for actual overflow. Both axes account for the space required by the other bar. Long text retains wrapping, keyboard scrolling and selection.

Pause AI always creates a hold. The single Resume AI action releases the hold and immediately retries saved recovery; with no saved recovery, it only releases the hold. Gameplay still requires the existing Cancel-default confirmation and process-instance approval. Retry resume preserves the same approval. Internal restore actions, CLI and journal formats remain unchanged.

Focused checks: dashboard 23 passed (three interactive cases excluded), tray 12 passed (one excluded), restore dialog three passed, presentation five passed, application 26 passed and command tracking four passed. The individually selected rendered-dashboard and DPI/GDI tests passed. The rendered fixture checks sibling hit testing, composited button pixels, overflow/refit, selection preservation and guarded Resume dispatch with a private mock state and no provider worker. Dark/light captures were inspected. GDI plateau remained 102/102. The full suite was not repeated for this ticket; the earlier full-suite result below predates these fixes. Formatting, Clippy/all targets and diff checks passed. Human appearance and live recovery acceptance remain pending.

## v1.0.0 candidate — 2026-10-05

This candidate is prepared for human review and remains unpublished. Use its final `SHA256SUMS.txt` and `BUILD-INFO.json` to identify installer/ZIP hashes and the compiled-source fingerprint. Appearance, comprehension, live LM recovery, actual power, installer/startup and complete performance checks were pending for that candidate. No live Ollama version range is claimed.

- Rust 1.99.0, committed lockfile: final integrated `cargo test --locked` passed 267 unit tests and ten CLI integration tests, with six ignored. The full suite ran once at candidate validation; the later tooltip-only adjustment used focused tests. Formatting, Clippy/all targets, diff checks, normal release build and release panic probe passed. The panic probe contained its expected injected discovery panic and retained subsequent refresh/logging.
- Dashboard/theme/tray focused checks passed: 23/two/11 tests respectively, with interactive tests excluded from those counts. Individually selected native dashboard resource/DPI, popup timer reentry, Ollama opt-in dialog and headless power-registration tests passed. The expanded light/dark drawing fixture warmed both themes at all DPI fonts for 98 relayouts, then measured another 49 at GDI count 102 before/after. Initial 49-warmup attempts had not reached that cache plateau. The measured growth tolerance was unchanged.
- Selection fixtures now draw actual text with leading whitespace to detect opaque backgrounds inside selected text extents. Both palettes restore DC state; native check/text/tab state, disabled colors and edit selection survive theme/resize changes. These checks do not establish physical monitor or screen-reader behavior. OS scrollbars, control borders, menus and Windows-owned dialogs retain OS rendering.
- Installer and portable ZIP compiled with signature-verified official Inno Setup 6.7.3 in private portable-tool mode. Package verification passes hashes, executable/CLI version 1.0.0, version-3 defaults, off-by-default experimental Ollama/Advanced, dependency declarations/license texts and absence of internal plans/reviews/runtime files. The initial all-platform license check found an irrelevant `r-efi` dependency; packaging now uses the resolved Windows target graph. The installer was compiled, not installed or executed.

### Private observation samples of the earlier candidate

Three startup-inclusive headless samples per variant requested 12 seconds and lasted 12.39–12.47 seconds. Parent CPU, working set and private memory were read every 250 ms on a 16-logical-CPU Windows PC. CPU share is parent CPU seconds / measured wall seconds / 16 × 100. All samples explicitly used observation, automatic control/notifications off, new private data directories, an absent fixture CLI and uncontacted loopback endpoints. No model/server request, game launch or recovery journal occurred. Real launcher metadata/process enumeration still contributed local discovery work.

Baseline is source `dc29db8`/0.3.5 rebuilt with the same Rust 1.99.0 compiler to compare source changes, not the historical distributed binary. Candidate CLI SHA-256 is `a1ae3e056d07774c6097f2021bcd0eac48395530c634d347a881357388e327a5`; baseline is `102c6f54df5a305aa2aeef2b1bd0a27800155a7d1a51184f870e1a84836ac376`. The measurement script retains raw private samples under ignored build output; only aggregate evidence is bundled.

| Variant (three samples each) | Total CPU share range | Peak working set range | Peak private memory range |
| --- | --- | --- | --- |
| Baseline, one unavailable LM route | 0.0865–0.0866% | 9.66–10.55 MiB | 2.32–2.52 MiB |
| Candidate, no enabled providers | 0.0865–0.1497% | 10.29–10.51 MiB | 2.66–3.32 MiB |
| Candidate, one unavailable LM route | 0.1102–0.1332% | 10.20–12.61 MiB | 2.75–3.19 MiB |
| Candidate, two configured uncontacted routes | 0.1102–0.1260% | 10.27–10.50 MiB | 2.91–3.25 MiB |

Both candidate executables are 3,597,824 bytes; baseline CLI is 2,835,456 bytes. Short parent-only samples are not steady-state budgets, healthy-provider control measurements or FPS evidence. Dashboard-open costs, native scan-gap/wakeup instrumentation, child-process CPU, simulated/actual gaming/restoration and the full paired performance matrix remain unmeasured here. Earlier provider/scanner fixtures establish bounded decisions and cooperative scheduling, not a hard native scan latency guarantee.

Human state comprehension, theme/accessibility, notifications under games/Windows suppression, live LM settings/server fidelity, actual sleep/wake, installer upgrade/uninstall and reboot/sign-in remain pending. Ollama is experimental with source/fixture evidence only. No release, tag, push, live provider/game, startup registration or application installation was performed.

## v0.3.5 stabilisation checks — 2026-10-04

- Windows x64, pinned Rust 1.98.1: 108 unit tests and 5 CLI process integration tests passed (four tests are ignored by default: two interactive tests and two subprocess fixtures exercised by ordinary regressions). The two interactive tests also passed explicitly. Checks include locked tests, formatting, Clippy (all targets/features), release build and feature-gated release panic probe. The probe catches an injected discovery panic, retains inventory, services a later refresh and confirms panic logging under the actual release profile.
- Automated cases cover journal writes before unload, partial unload/inventory/restore/read-back failures, failed stage/final persistence, restart from each destructive stage, observation/pending/game guards, nondefault port and server start/stop failure, and closing a temporary control server after an interrupted restore.
- CLI child-process tests cover busy instance locks, observation/corrupt-journal refusals, conflicting modes, live versus stale status, and structured doctor output with missing CLI/unwritable storage. Local WS fixtures check protocol logging and deadlines despite ping traffic; subprocess fixtures check inherited pipes and oversized output. These fixtures never connect to LM Studio.
- Interactive popup/timer reentry passes. Interactive dashboard font and fixed-row scaling passes across 100/150/200% synthetic relayouts; after native caches warm up, another 49 relayouts show stable GDI usage. This checks native controls/resources, not physical multi-monitor visual appearance.
- Full dark client controls and an embedded doctor view are deferred. Captions use the app-theme preference, track setting changes and preserve high contrast; classic client controls remain system-coloured.

A read-only smoke test against the installed CLI (commit 69d945a) reported the HTTP server stopped, writable private report storage, and WS compatibility correctly unknown; it did not start the server or unload models. No live v0.3.5 unload/restore/gameplay or installer upgrade/uninstall tests were performed. GitHub Windows CI and release build passed for implementation commit `8b649f4` ([build 37209621271](https://github.com/Vkuparin/gamepause-for-local-ai/actions/runs/37209621271)). The build compiled the portable ZIP and Inno Setup installer with pinned Rust 1.98.1. Downloaded artifacts matched both SHA-256 checksums; both packaged executables reported product version 0.3.5, dependency notices/licenses were present, and internal review/plan documents were absent. Live lifecycle, physical desktop appearance and installer upgrade/uninstall checks remain publication gates. Historical v0.2.0 lifecycle and performance results below retain their original versions and dates.

Results below describe v0.2.0 tested on Windows with LM Studio 0.4.25 on 2026-10-03, with earlier release evidence identified separately. Automated tests, metadata compatibility, live behavior, and benchmarks are separate evidence.

## Automated checks

42 tests pass (plus one interactive desktop test excluded from the default suite). Cargo formatting and Clippy checks pass with warnings treated as errors. Coverage includes validated configuration, atomic replacement, KeyValues/protobuf parser boundaries, executable path boundaries, nested installations, helper exclusions, startup command quoting, equivalent model identity representations, exact settings comparisons, and recovery behavior.

New v0.2.0 cases cover migration to automatic defaults, remembering an explicitly disabled setting, live settings persistence/rejection, searchable dashboard rows and recognition details, exact executable registration, newly installed Steam metadata without restarting, one-shot unfamiliar-process refresh, background utility and REDprelauncher exclusions, temporary-server cleanup on capture success/failure, recovery while automatic pausing is off, remembered game paths across restart, initial discovery gating, and clearing abandoned capture errors.

Recovery tests cover multiple models, empty snapshots, observation, grace periods, switching games, disabled detection, manual holds, journal-before-unload ordering, partial unload, partial restore after restart, failed server shutdown, game restart before/between restoration, and unknown/corrupt journal stages.

These tests do not prove compatibility with every launcher version or future LM Studio protocol.

## Tray crash regression (0.1.1)

The original 0.1.0 tray callback was reproduced in a private diagnostic build: leaving the popup open until the next timer tick caused a RefCell borrow panic and Windows exit code 0xc0000409, matching the reported crash. The fixed callback releases the UI-state borrow before entering Windows menu tracking and other native calls.

The regular unit suite holds a menu session open while invoking the actual timer callback, verifies error-state updates, rejects nested menu openings, and verifies reopening after dismissal. A separate interactive Windows test opens three distinct native popup menus and leaves each open across at least two timer callbacks before dismissal. It uses observation-only test state and does not contact LM Studio. The earlier Steam acceptance test below covers the original model lifecycle; this patch targets tray reentrancy.

## Local discovery compatibility

The v0.2.0 inventory found 37 game locations across Steam, Epic, Xbox, EA, Ubisoft Connect, and Battle.net using this PC's launcher metadata. Known background utilities are now filtered out (the earlier inventory contained 41 locations). No adapter errors were reported. This checks discovery, not every game's process behavior or a full live lifecycle on every launcher.

## Installed v0.2.0 Steam acceptance tests

The per-user installed app was upgraded and started through its normal background startup command. Automatic pausing was on through normal configuration migration/defaults. No active-mode argument, edited game list, or test-only detection bypass was used. The dashboard was closed during gaming.

The Witcher 3 was launched using Steam's Play button and REDlauncher's Play button, reached its main menu, and exited using its normal Exit menu action. Two complete cycles passed:

- With the HTTP server initially running, the currently loaded chat model was captured, unloaded, and the server stopped. After game exit and the normal 30-second delay, the model and running server returned and recovery cleared.
- With the HTTP server initially stopped and the model still loaded, capture temporarily started the server. During gaming the model inventory was empty and the server stopped. Automatic restoration returned the model and left the server stopped; recovery cleared. The server was then returned to the user's pre-test running state.
- An independent read-only verifier compared the restored variant, identifier, TTL policy, 98,304-token context, four parallel slots, all 11 current raw load fields, and all exposed native configuration fields against the baseline after each cycle. Both comparisons passed. This test used the user's one currently loaded model; it did not reintroduce the earlier embedding model.
- The real launch flow exposed a REDprelauncher helper being treated as a game. It is now excluded, covered by a regression test, and the corrected launcher-only state was checked before starting the actual game. Earlier busy-capture errors were also corrected to clear after an abandoned session with no journal.

The optimized interactive tray regression passed again: three native popup openings, each spanning at least two timer callbacks. The installed dashboard's search, recognition details, ignore/re-enable and automatic-pausing controls were checked. Running-app rows populated on demand. The optional restore delay was saved as 31 seconds without restarting and then returned to 30 seconds. The standard executable picker opened and cancelled without changing registration. Full executable registration is covered by unit tests. Optional controls remain hidden until Settings is opened. Opening the already-running installed app showed the dashboard without a second watcher.

These were game-menu sessions. No FPS/frame-time gain is claimed. Custom identifiers and non-null TTL restoration are implemented but have not been exercised in the live game tests. Complete live lifecycle coverage remains Steam only. New-installation behavior is tested with metadata fixtures, not by downloading a new game during this test.

## Earlier v0.1.0 Steam acceptance test

The Witcher 3 was launched through Steam and REDlauncher, reached the main menu, and exited normally. REDlauncher alone did not trigger pausing; the actual game process did.

- Captured a chat model with Q4_K_M quantization, 98,304-token context, four parallel slots, and 25 raw load fields, plus an embedding model with 2,048-token context.
- Unloaded both instances and stopped LM Studio's HTTP server.
- The first Rust restore exposed a CLI identity representation difference and a native-inventory association issue when loading by variant. The journal retained both originals. The loader was corrected to require the saved variant and load through the catalog key, with exact identity/configuration verification afterward. Recovery succeeded.
- The complete Steam launch/exit test was repeated with the corrected native watcher. Both models restored automatically after the 30-second delay. Every captured raw key/value setting and exposed native configuration field was verified, the original identifiers/TTL policies were retained, the server returned to running, and recovery cleared.
- A live background inference request caused pause to defer until idle. This is intentional: GamePause does not interrupt inference to force a pause. Continuous AI activity can therefore delay VRAM release; pause background agents before gaming.

Only a game-menu session was exercised. No FPS/frame-time gain is claimed. Custom identifiers and non-null TTL restoration are implemented but have not been exercised in the live game test. Launcher installations were inspected from all six launchers; complete live lifecycle coverage is Steam only.

## Watcher overhead

CPU share = process CPU seconds / wall seconds / logical CPU count × 100. Working set is recorded separately. Samples included a 30-second local metadata refresh. Child process CPU, initial Xbox package discovery, and model loading are outside these process-only samples. Heavy Xbox package refreshes defer while gaming.

| Implementation | Sample | CPU seconds | Logical CPUs | Total CPU share | Maximum working set |
|---|---:|---:|---:|---:|---:|
| Installed v0.2.0, dashboard closed | 46.31 s | 0.4062 s | 16 | 0.0548% | 14.07 MiB |
| Native Rust preview | 45.21 s | 0.5469 s | 16 | 0.0756% | 15.72 MiB |
| Earlier private Python prototype | 45.28 s | 1.6094 s | 16 | 0.2221% | 47.42 MiB |

These samples came from the same Windows 11 / RTX 5090 PC in the Witcher 3 menu. They are short observations, not a controlled cross-machine benchmark. The v0.2.0 sample used a freshly started background watcher, with its dashboard closed, and included a local inventory refresh. The older native/Python samples describe earlier builds. A paired gameplay frame-time benchmark remains separate work.

## Distribution validation

The per-user installer, Start menu entry, executable version/icon metadata, sign-in Run entry, and uninstall cleanup are checked locally. Config/recovery retention is verified separately from removal of installed files. Startup registration is checked; this is not a reboot/sign-in acceptance test.

The final portable executable was inspected to import only Windows system DLLs, without a separate Visual C++ runtime dependency. Binary releases are unsigned previews. They include documentation, third-party licenses, and SHA-256 checksums. CI checks the tracked Rust source on Windows; the release workflow builds tagged distributions.
