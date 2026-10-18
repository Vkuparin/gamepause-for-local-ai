# Hermes local runtime review

Reviewed 2026-10-14; live acceptance updated 2026-10-15 for GamePause v2.6.0. Implementation uses Hermes's native local-runtime lifecycle API, not generic process termination.

## Evidence

The initial source review pinned NousResearch/hermes-agent commit `e8c8e464715d11bfb7b3006edabff6b719a29b8b`. The owner's installed source has the same lifecycle/configuration handlers reviewed for this contract. A separately verified application release version is unavailable; no Hermes release range is claimed.

- [Local-model lifecycle handlers](https://github.com/NousResearch/hermes-agent/blob/e8c8e464715d11bfb7b3006edabff6b719a29b8b/hermes_cli/web_routers/local_models.py): native stop disables the runtime persistently; native start enables it and boots the installed engine.
- [Router supervisor](https://github.com/NousResearch/hermes-agent/blob/e8c8e464715d11bfb7b3006edabff6b719a29b8b/hermes_cli/local_runtime/supervisor.py): bearer-authenticated model inventory/load, demand autoload and crash supervision.
- [Host rendezvous](https://github.com/NousResearch/hermes-agent/blob/e8c8e464715d11bfb7b3006edabff6b719a29b8b/gateway/host_rendezvous.py): owner records, birth fingerprint, private token and authenticated host identity.
- [Configuration GET](https://github.com/NousResearch/hermes-agent/blob/e8c8e464715d11bfb7b3006edabff6b719a29b8b/hermes_cli/web_routers/config_env.py): `include_defaults=false` uses the saved configuration read path. GamePause extracts only model/runtime preferences for a digest and does not persist the full response.

Live read-only checks verified custom data-directory discovery, the default-profile Desktop backend, native endpoint ownership, control authentication, the saved enabled state, and one resident `Qwen3.8-27B-UD-Q4_K_M` model under CUDA llama.cpp `b11370`. The actual live context was `147456`; the UI's saved override was `131072`. Busy/queue evidence showed active inference during inspection. No model, runtime or user setting was changed.

On 2026-10-15 the owner authorized a quick live check and release after success. The final Hermes-only CLI round-trip passed native shutdown, verified process absence, startup, model replay and supported-field verification. An independent comparison found the runtime enabled, Qwen3.8-27B-UD-Q4_K_M loaded at context 147456, saved model/runtime preferences unchanged and recovery cleared. Initial attempts exposed retained exited-process handles and asynchronous load acknowledgements; both were fixed with regressions before the passing cycle. No game was launched or startup registration changed. GPU memory was not independently sampled. Multi-model, desktop persistence across reboot and physical/screen-reader UI acceptance remain untested.

## Implemented contract

The first adapter accepts a local protocol-1 default-profile backend with complete supported evidence. It refuses named/multiple profiles, foreign/stale owners, ambiguous endpoints and independently operated model servers. It checks the native process birth time, user/session and listening PID before using a token. Records omitting the data directory are checked against the backend's bounded native `HERMES_HOME` evidence.

Capture records runtime enabled/running state, router/model-process birth identities, both original endpoints, model IDs, model/companion SHA-256 digests, engine digest and promised live fields. Supported fields are context, batch/ubatch sizes, K/V cache types, flash attention and available speculative type/draft-token limit. Files remain read-locked during the hold; the snapshot contains no credentials or raw command. Each file is capped at 128 GiB and a 90-second hashing allowance. Model counts, JSON, metrics and native metadata reads are bounded.

The original snapshot is immutable. Stop, absence verification, start, each model load and final restoration verification are separate coordinator units. Each mutation follows an atomic checkpoint. Lost responses retry through fresh state, and acknowledgements alone never complete pause/restore. Partial failures retain the journal while unrelated providers can finish. Recovery envelope 3 carries Hermes snapshot 1; settings 5 adds a guarded v4 backup.

Pause uses slots and both processing/deferred metrics. Loading or unknown inventory is deferred. Stop uses `/api/local-models/server`; GamePause never terminates the Hermes desktop/router itself. Pause completion checks runtime disabled plus recorded process exit and exact engine-process absence. Restore uses the native start route followed by one `/models/load` per captured resident, then read-back verification. It never sends inference, downloads, activates a default or edits presets.

Exited process objects are ignored even while Windows retains their handles. A successful load acknowledgement is followed by bounded inventory polling for readiness before the next checkpoint and final verification.

An off/absent runtime has no startup obligation. A running-but-disabled runtime is refused before mutation because native start cannot faithfully reproduce it. Empty running runtimes have a lifecycle obligation but no model loads. Changed model/runtime preferences, content or supported fields refuse completion. The API cannot distinguish a user's manual off from GamePause's off when both leave identical state; the user approved automatic restoration of the recorded hold, and Usage discloses this limitation. External re-enable can override a completed pause, which stays quiet.

## Verification

Focused mock tests cover checkpoint replay after restart, absence verification failure, immutable capture, queued-binding refusal, four-provider journal persistence, guarded settings migration and provider-row ordering. Existing dispatcher/coordinator, recovery and dashboard tests exercise the shared scheduling and presentation paths. The offscreen dashboard tests cover compact/normal/large sizes and 100/125/150/175/200% scale. The final full suite passed 347 library and nine CLI tests; six interactive/child fixtures stayed ignored. The focused live cycle above was also exercised.

Final check and package outcomes are recorded in [Validation](VALIDATION.md#v260-hermes---2026-10-15). The user requested focused tests rather than exhaustive scenario coverage; multi-model live replay, sleep/reboot, installer upgrade, Windows 10 and screen-reader behavior remain unexercised.

## Gateway-owned runtime follow-up, 2026-10-18

The installed application's log reported “Hermes runtime has no verified native control owner” during manual pause. The router's recorded owner was the default-profile messaging gateway, while the verified Desktop HTTP backend was a different process. The original adapter required those owners to be identical. The runtime still had one model worker beneath one router.

The installed native stop handler only stops its own supervisor or a recorded orphan; it refuses another live owner. The gateway's control socket has no runtime-only lifecycle verb. The owner authorized closing/reopening the gateway when runtime-only control is unavailable. The v2.6.1 fallback verifies the gateway record, native identity, user/session, data directory and supported launch form. It seals command, working directory and bounded environment together; model, engine and gateway-image digests remain immutable. Gateway stop/start each gets a separate checkpoint around the existing native lifecycle and replay units. A changed or automatically restarted gateway retains recovery. The Desktop/backend must stay open for native runtime settings/control.

Read-only diagnostics verified native control plus the gateway restart requirement and readable supported restart details. The owner subsequently authorized a live test. After fixes for asynchronous worker exit, exited-process inventory and mutable startup environment, replay of the captured gateway-fallback obligation passed through the real watcher with a harmless registered process. Gateway/engine absence and disabled state were independently observed during pause; automatic restore returned the gateway, original model, context 221184 and saved preferences, then cleared recovery. The captured command/directory and verified home/image bind the resumed gateway; its live environment can change during startup, while the original encrypted environment is still used for relaunch. See Validation for the failed attempts, retained recovery and precise replay scope.
