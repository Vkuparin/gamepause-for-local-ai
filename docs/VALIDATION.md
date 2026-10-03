# Validation

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
