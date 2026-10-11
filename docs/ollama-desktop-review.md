# Ollama Desktop lifecycle review

Reviewed on 2026-10-12 for [issue #1](https://github.com/Vkuparin/gamepause-for-local-ai/issues/1) and release 2.5.3.

## Decision and scope

Ship a single opt-in setting under Advanced > Ollama. Native lifecycle control is supported only for positively verified Windows Ollama Desktop 0.40.3. Unknown versions, standalone servers, WSL/Docker and ambiguous ownership keep ordinary model unloading. The owner explicitly accepted possible request cancellation on 2026-10-12, replacing the issue's interruption-free requirement.

An empty model inventory is a point-in-time observation. A client can begin a request before Desktop handles Quit. Repeating inventory checks cannot make inventory and shutdown atomic. GamePause therefore waits for existing captured inference through unloading/absence verification, but cannot promise completion of a request arriving during shutdown. The UI and usage guide disclose this limitation. External restarts can also let clients reload models; a completed pause does not poll for or terminate those restarts.

## Pinned upstream evidence

Source review first covered v0.40.2 at `b061384d90ff455462bc32745dd0de479de717a3`; installed/live evidence is for v0.40.3 at `eab97e9f92b9a25c2d52d2cc6c1b1c99bd9fae21`.

- [Tray implementation](https://github.com/ollama/ollama/blob/eab97e9f92b9a25c2d52d2cc6c1b1c99bd9fae21/app/wintray/tray.go) creates the `OllamaClass` window.
- [Window messages](https://github.com/ollama/ollama/blob/eab97e9f92b9a25c2d52d2cc6c1b1c99bd9fae21/app/wintray/eventloop.go) route `WM_COMMAND` command 9 to Quit.
- [Desktop startup](https://github.com/ollama/ollama/blob/eab97e9f92b9a25c2d52d2cc6c1b1c99bd9fae21/app/cmd/app/app_windows.go) supports `hidden` and `--fast-startup`. Fast startup avoids taking the ordinary pending-upgrade path during recovery.
- [Managed-server shutdown](https://github.com/ollama/ollama/blob/eab97e9f92b9a25c2d52d2cc6c1b1c99bd9fae21/app/server/server.go) can forcibly terminate its server after the grace period or signal failure. GamePause never directly terminates the server.
- [Inference server](https://github.com/ollama/ollama/blob/eab97e9f92b9a25c2d52d2cc6c1b1c99bd9fae21/server/routes.go) closes HTTP and unloads runners on its shutdown signal; it does not establish an inference-draining contract.

These private implementation details are version-gated, not a public Ollama lifecycle API. The request race is an inference from the separate inventory and Quit operations.

## Production implementation

`src/ollama_session/desktop.rs` owns lifecycle evidence and phases. Private native children perform bounded Windows operations on the existing control thread. No window handle enters the engine/UI bridge. No helper executable, runtime dependency, permanent worker, listener, proxy, idle inventory request or repeated shutdown is added.

Capture verifies the unique tray owner, executable paths, process creation times, current Windows account/session, server parent and TCP endpoint owner across IPv4/IPv6. Desktop CLI and server API versions must both be 0.40.3. Startup arguments are restricted to reviewed background flags. Both executable files are locked against write/delete sharing and hashed with native SHA-256. The command, working directory, full application environment and effective server configuration are sealed with per-user DPAPI before entering the recovery journal. Commands/environment values are never printed or persisted in plaintext.

Model unloading retains the existing supported subset and expiry policies. Stop, stopped verification, start and readiness verification are separately checkpointed provider units. Shutdown completion requires both captured processes to have exited and the port to have no listener. Original model and lifecycle evidence remain immutable through retries and partial failures.

Recovery rechecks executable hashes and startup evidence before execution, reconciles an already-running matching Desktop, and verifies its effective server environment and original endpoint before model restoration. An externally restarted instance can satisfy recovery but never conveys fresh stop authority. Startup refuses other Desktop windows, matching processes still starting, occupied ports and any remaining `ollama.exe` process: Desktop's own startup can reap standalone servers. A third-party launch after the last checks remains a concurrency limit; GamePause's service claim excludes cooperating instances, not unrelated applications.

Ordinary captures stay at Ollama snapshot version 1. Opt-in lifecycle/fallback captures use guarded version 2 within unchanged recovery envelope 3 and settings version 4. Older snapshots retain their expiry policy. Unknown formats fail closed. A lifecycle capture counts as an obligation even with no resident model. Pending recovery blocks changing the opt-in choice.

## Live evidence

The owner authorized live tests. Ollama was updated by the owner from 0.40.0 to 0.40.3; the agent downloaded no runtime or model. Tests used private Desktop profiles, an isolated loopback port and private recovery/settings files. Model tests copied the already-installed small GGUF model's artifacts to a fixture directory. LM Studio was disabled. The original Desktop began with empty residency and was restored with empty residency after each completed experiment. No real GamePause recovery journal was read or overwritten.

The feature-gated Rust probe requires `--run`. It seals and flushes original startup evidence before temporarily closing an empty original Desktop, verifies native Quit and managed-server exit, and restores the original afterward. Its dedicated `--recover-test` path can restore retained startup evidence after a harness failure, first proving any current Desktop belongs to that private profile.

| Scenario | Observation |
|---|---|
| Initial empty private Desktop | Native Quit, app/server exit and endpoint unavailability verified; hidden restart verified twice |
| Production empty capture | Pause 457 ms; fresh adapter restored from persisted journal in 4,807 ms |
| Active inference with qwen2.5:0.5b | Request completed; pause waited 33,260 ms before verified shutdown |
| Production model recovery after adapter replacement | Restore 7,188 ms; identity/context 4096 and indefinite residency verified; journal cleared |
| Late request near Quit | Request failed during shutdown; fresh-adapter model recovery completed in 6,432 ms |
| External private Desktop restart | Existing PID preserved, no duplicate launch; recovery 512 ms |
| Native scanner and engine custom-game cycle | See release benchmark results below; only the harmless fixture was supplied to the scanner |

The initial full-watcher harness correctly held recovery for an unrelated running real game. Ignoring that game did not bypass remembered-game guards. The harness was changed to give the native scanner/engine only the harmless custom game, without changing application policy. Another harness check mistook the atomic `null` journal after successful recovery for a pending file; it was corrected and the original Desktop recovered through the sealed test record. The production adapter did not need a workaround. Failed full-watcher fixture journals remain isolated test evidence.

Earlier proof runs used no models. They do not establish broad model compatibility. Live model coverage remains one local GGUF completion model per run; embedding, vision, multiple models and mixed live LM Studio recovery are not exercised. Real Windows reboot/sleep, foreign-account ownership, executable replacement during a live Desktop run, and arbitrary third-party startup races are not live-tested.

## Automated acceptance

Fixtures cover each checkpoint, lost acknowledgements across native phases, retained original evidence, fresh-adapter recovery, unknown snapshot versions, false completion, unsupported fallback, absent service, client reloads, no work after completed pause, external restart authority and a new game during Desktop startup. Native fixtures check process creation-time guards, current-account identity, repeatable SHA-256/changed content and effective environment normalization/drift. Existing mixed-provider and LM Studio/process tests remain applicable.

Final ordinary suite: 337 library tests and nine CLI integration tests passed; six interactive/child fixtures remained ignored as standalone tests. Formatting, Clippy for all targets/default features and all features, and diff checks are required for the final source. Fixture network/atomic-write restrictions required running the suite outside the sandbox. No ignored live/UI tests were run as a batch.

## Release performance

Compare the published 2.5.2 GUI binary to the 2.5.3 release candidate on the same Windows PC. Two headless observation samples per variant used private profiles and disabled providers; these measure startup to first `status.json` and five seconds of idle CPU after four seconds of warmup. They do not measure an open dashboard or active inference.

| Measure | 2.5.2 | 2.5.3, setting off | 2.5.3, setting on |
|---|---:|---:|---:|
| Executable bytes | 7,757,824 | 7,888,896 | 7,888,896 |
| Startup to first status, ms | 261.8 / 68.4 | 196.3 / 72.0 | 71.4 / 87.5 |
| Idle CPU seconds per 5-second sample | 0.0781 / 0.0781 | 0.0781 / 0.0625 | 0.0781 / 0.0781 |
| Private memory, MiB | 2.90 / 2.89 | 3.06 / 3.18 | 3.07 / 2.98 |
| Working set, MiB | 18.71 / 13.01 | 19.14 / 13.22 | 13.25 / 13.22 |

Size grew 131,072 bytes (128 KiB, 1.69%). Idle CPU did not increase in these short samples. Startup has cache/order noise; the data does not establish a speed improvement. Private memory grew about 0.1–0.3 MiB. Source inspection confirms no new idle worker/poll. Dependency versions and lockfile entries are unchanged apart from the application version; native Windows API features were enabled.

Pause/resume comparison uses a release-mode probe linked against unchanged provider/engine/scanner source at tag v2.5.2, then against 2.5.3, with the same copied model, context 4096, indefinite residency and harmless custom game. The current release GUI binary supplied the observation preflight. Baseline model pause/restore took 152/1,567 ms; Desktop protection took 2,391/6,897 ms. The additional 2.24/5.33 seconds include verified service shutdown and readiness. One cycle per source on one PC is latency evidence, not a general performance bound.

To repeat a live probe explicitly:

```powershell
cargo run --locked --release --example ollama_desktop_probe --features ollama-desktop-test -- 'C:\Users\Example\AppData\Local\Programs\Ollama\ollama app.exe' --run
```

`--with-model` requires a private copy of the locally installed qwen2.5:0.5b artifacts in `GAMEPAUSE_FIXTURE_MODELS`. `--external-restart` exercises reconciliation. `--engine-game` exercises the native scanner, engine, runtime/claims and real schema-3 journal with a harmless executable. The direct coordinator probe replaces the adapter during a pause to exercise persisted lifecycle recovery. `--race-request` sends a late request near native Quit. Do not use these flags with unreviewed profiles or leave original-model restoration to an abrupt test-process exit.

## Changed files

| Area | Files |
|---|---|
| Lifecycle | `src/ollama_session.rs`, `src/ollama_session/desktop.rs`, `src/ollama_session/desktop/native.rs`, `src/ollama_session/desktop/native/evidence.rs`, `src/ollama_session/desktop/native/startup.rs` |
| Routing and recovery | `src/provider_runtime.rs`, `src/recovery.rs`, `src/ownership.rs`, `src/engine.rs` |
| Startup sealing | `src/process_session.rs`, `src/process_session/sealing.rs` |
| Settings and UI | `src/config.rs`, `src/dashboard/settings.rs`, `config.example.json` |
| Tests | `src/ollama_session/tests.rs`, `src/ollama_session/desktop/tests.rs`, native child tests, `src/config/tests.rs`, `src/engine/tests.rs`, `src/process_session/tests.rs`, ownership tests |
| Live harness | `examples/ollama_desktop_probe.rs`, `examples/ollama_desktop_probe/existing.rs` |
| Documentation | `README.md`, `SECURITY.md`, `AGENTS.md`, `docs/CONFIGURATION.md`, `docs/DEVELOPMENT.md`, `docs/USAGE.md`, `docs/VALIDATION.md`, this review |
| Release | `Cargo.toml`, `Cargo.lock`, `installer/GamePause.iss`, `CHANGELOG.md`; Scoop manifest updated after published ZIP verification |
