# Validation

Results below describe the native Rust preview tested on Windows with LM Studio 0.4.25 on 2026-10-03. Automated tests, metadata compatibility, live behavior, and benchmarks are separate evidence.

## Automated checks

27 tests pass. Cargo formatting and Clippy checks pass with warnings treated as errors. Coverage includes validated configuration, atomic replacement, KeyValues/protobuf parser boundaries, executable path boundaries, nested installations, helper exclusions, startup command quoting, equivalent model identity representations, exact settings comparisons, and recovery behavior.

Recovery tests cover multiple models, empty snapshots, observation, grace periods, switching games, disabled detection, manual holds, journal-before-unload ordering, partial unload, partial restore after restart, failed server shutdown, game restart before/between restoration, and unknown/corrupt journal stages.

These tests do not prove compatibility with every launcher version or future LM Studio protocol.

## Local discovery compatibility

The Rust inventory found 41 installed locations across Steam, Epic, Xbox, EA, Ubisoft Connect, and Battle.net using this PC's launcher metadata. No adapter errors were reported. This checks discovery, not every game's process behavior or a full live lifecycle on every launcher.

## Steam acceptance test

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
| Native Rust preview | 45.21 s | 0.5469 s | 16 | 0.0756% | 15.72 MiB |
| Earlier private Python prototype | 45.28 s | 1.6094 s | 16 | 0.2221% | 47.42 MiB |

Both samples came from the same Windows 11 / RTX 5090 PC in the Witcher 3 menu. They are short observations, not a controlled cross-machine benchmark. Runtime resources, C runtime linking, and error handling received small changes afterward; these figures describe the sampled native build. A paired gameplay frame-time benchmark remains separate work.

## Distribution validation

The per-user installer, Start menu entry, executable version/icon metadata, sign-in Run entry, and uninstall cleanup are checked locally. Config/recovery retention is verified separately from removal of installed files. Startup registration is checked; this is not a reboot/sign-in acceptance test.

The final portable executable was inspected to import only Windows system DLLs, without a separate Visual C++ runtime dependency. Binary releases are unsigned previews. They include documentation, third-party licenses, and SHA-256 checksums. CI checks the tracked Rust source on Windows; the release workflow builds tagged distributions.
