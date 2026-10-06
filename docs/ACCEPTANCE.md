# Candidate acceptance

## v2.0.1 owner publication request - 2026-10-06

The owner instructed "push and publish 2.0.1 release according to the process" after receiving the stabilization fixes, their check results and the list of unverified behavior. This authorizes pushing and publishing v2.0.1. The owner did not supply a test report for this build, so this record is a publication request, not a statement that the owner ran it.

Automated checks passed: 298 library tests and 10 CLI integration tests (6 ignored), formatting, Clippy, the release resilience probe and the release build. The package verifier ran in the release workflow.

Exercised on one Windows 11 PC: the isolated dashboard review fixture and the interactive tray menu test, both with fictional or observation-only state; a tray menu capture from an instance with a private data directory and LM Studio and Ollama disabled; and a four-minute side-by-side run of 2.0.0 and 2.0.1 in headless observation mode with private data directories (4.95 s and 3.19 s of CPU time).

Not exercised for v2.0.1: any live Ollama or LM Studio session, so the longer Ollama load timeout has fixture coverage only; the corrected tray icon color on a real tray; the notification rules across a real sleep and wake; the Advanced draft, Escape prompt and Ignore executable change beyond unit tests and rendered fixtures; the process provider with real llama.cpp or KoboldCpp; real games, Windows 10, install/upgrade/uninstall, startup, multi-monitor and screen-reader behavior. Executables and installer are unsigned. Published SHA256SUMS.txt and BUILD-INFO.json identify the packaged artifacts; the release acceptance asset records this request separately from the build-time review marker.

## v2.0.0 owner publication request - 2026-10-06

The owner set the goal "keep on implementing the 2.0.0 features, and after you are done, push to publish and release according to the process." This authorizes committing and publishing v2.0.0. The owner did not supply a test report for this build, so this record is a publication request, not a statement that the owner ran it.

Automated checks passed: 291 library tests and 10 CLI integration tests (6 ignored), formatting, Clippy, the release resilience probe and the release build. The package verifier ran in the release workflow.

Exercised live during development, on one Windows 11 PC with private data directories and a renamed system executable registered as a game:

- Ollama 0.35.1 with one local GGUF model: observation mode, pause and restore with the frozen keep-alive, indefinite keep-alive, a pause requested during a running generation, recovery after the watcher was killed mid-pause, nothing loaded, nothing listening on the port, and migration of earlier settings.
- LM Studio with one small embedding model loaded: the round-trip test alone and together with Ollama, and a mixed LM Studio plus Ollama session through the watcher. LM Studio reported as not installed (pointed at a missing CLI path) with and without Ollama.
- The memory-freed figure, the ask rule's no-pause path and message, and the global shortcut end to end through the tray app with a synthetic key press.
- The process provider's Windows handling with a stand-in executable: stop, sealed journal, relaunch with the same command line, and stop-only mode.

Not exercised: the dashboard on a real desktop beyond rendered fixtures (the new Other apps page, the ask and suggestion prompts, the shortcut field), the ask rule's accept path, game suggestions against any real fullscreen program, real llama.cpp or KoboldCpp, Ollama versions other than 0.35.1, Ollama embedding or vision models and several resident models, real games, Windows 10, install/upgrade/uninstall, startup, sleep/wake, multi-monitor and screen-reader behavior. Published SHA256SUMS.txt and BUILD-INFO.json identify the packaged artifacts; the release acceptance asset records this request separately from build-time review status. Earlier records below are historical.

## v1.7.0 owner publication request - 2026-10-06

The owner instructed "commit and publish v1.7.0" after receiving the slimmed tray menu, its check results and the list of unexercised behavior. This authorizes committing and publishing v1.7.0. The owner did not supply a test report for this build, so this record is a publication request, not a statement that the owner ran it.

Automated checks passed: 262 library tests and 10 CLI integration tests, formatting, Clippy, the release resilience probe and the release build. The package verifier passed. One Ollama mock-HTTP engine test failed once during development and once in the release workflow's first attempt because of a timing race in the test fixture; the workflow rerun of the same commit passed, and the fixture was corrected after the tag.

Not exercised for v1.7.0: the new tray menu on a real desktop (appearance, width, keyboard and screen-reader traversal, high contrast), the ignored interactive popup test, the real application against LM Studio or Ollama, real games, Windows 10, install/upgrade/uninstall, startup, sleep/wake and multi-monitor behavior. Ollama remains experimental and off by default. Published SHA256SUMS.txt and BUILD-INFO.json identify the packaged artifacts; the release acceptance asset records this request separately from build-time review status. Earlier records below are historical.

## v1.6.0 owner publication request - 2026-10-06

The owner instructed "commit, and publish v1.6.0 according to the repo instructions" after receiving the restyled dashboard, its fixture renders and the list of unexercised behavior. This authorizes committing and publishing v1.6.0. The owner did not supply a test report for this build, so this record is a publication request, not a statement that the owner ran it.

Automated checks passed: 261 library tests and 10 CLI integration tests, formatting, Clippy and the release build. The isolated rendering fixture and the dashboard lifecycle fixture passed individually with fictional state. The package verifier passed.

Not exercised for v1.6.0: the real application against LM Studio or Ollama, real games, executable-icon display for real game installations beyond a unit test against a Windows system executable, the visible title-bar tint, Windows 10, the installer's tray-only launch, upgrade/uninstall, startup, sleep/wake, multi-monitor and screen-reader behavior. Ollama remains experimental and off by default. Published SHA256SUMS.txt and BUILD-INFO.json identify the packaged artifacts; the release acceptance asset records this request separately from build-time review status. Earlier records below are historical.

## v1.5.0 owner acceptance - 2026-10-05

The owner reported "OK, commit and publish, it seems to work now" after checking the redesigned app. This authorizes committing and publishing v1.5.0. Automated checks passed: 257 library tests and 10 CLI integration tests, formatting, Clippy and the release build. The isolated rendering and dashboard lifecycle fixtures also passed individually.

This feedback does not establish new live LM Studio/Ollama, installer-upgrade, startup, sleep/wake, multi-monitor or screen-reader coverage. Ollama remains experimental and off by default. Published SHA256SUMS.txt and BUILD-INFO.json identify the packaged artifacts; the release acceptance asset records approval separately from build-time review status. The v1.0.0 records below are historical.

## Owner acceptance - 2026-10-05

The owner reported "Human tests passed" and explicitly authorized pushing and publishing v1.0.0. This approval applies to the final appearance review build below. The installer and ZIP are published unchanged. Later versions will focus on further UI and UX improvements.

- Installer SHA-256: `2fe1061702516eec3227d408ea988ca57e934cd04176ee0c705122e0fcd0f1b6`.
- ZIP SHA-256: `8fc85ed1e29e5ccf2851f8903760cf5416d784cb1f265bec0b67ff36f35eae0f`.
- Source fingerprint: `59cf957fde1eae66594731c5ff02dfff7100bc1f2ce094e213a4a08ba036c3a9`.

The owner did not provide a scenario-by-scenario report or environment details. Do not infer specific provider versions, model identities, accessibility, power, startup or installer-upgrade coverage from this approval. Ollama remains experimental and off by default, with no live compatibility range claimed. Earlier pending-review entries below record the state before this approval and do not override it. Bundled candidate documentation and BUILD-INFO describe the build-time review status; this record and the release acceptance asset record the subsequent approval.


GamePause 1.0.0 is a review candidate, not an accepted release. Test the final installer or portable ZIP identified by `SHA256SUMS.txt` and `BUILD-INFO.json`. Record the exact hashes, Windows build, LM Studio application/CLI versions, model identities and original server state. Do not substitute earlier preview results. Ollama has no live compatibility acceptance and remains experimental/off by default.

## Review record

| Evidence | Result |
| --- | --- |
| Candidate artifact/hash and build fingerprint | Copy from the delivered build files |
| Windows build, display DPI and theme | Awaiting human review |
| LM Studio application and CLI versions | Awaiting live acceptance |
| Loaded LLM/embedding models, identifiers, TTL and settings | Awaiting live acceptance |
| Original server running state and port | Awaiting live acceptance |
| Human reviewer/date and feedback | Pending |
| Signing | Unsigned candidate |
| Publication | Blocked until review, feedback/fixes and a separate publishing instruction |

## Appearance and state understanding

Use the delivered candidate, with Advanced initially off. For each watching, busy inference, capture/unload, confirmed pause, countdown, restore, manual hold, unavailable detection, partial failure and confirmed gameplay coexistence state, ask the tester to explain: which games were detected, what each enabled provider is doing, why, what happens next and which action is valid. Confusion or a false success message is a defect; record it without relying on logs or Advanced.

| Scenario | Expected result | Human result |
| --- | --- | --- |
| Header and simultaneous games | Correct version; all recognized processes and named enabled-provider outcomes visible | Pending |
| Follow Windows/forced Light/forced Dark/high contrast; selected game | All controls visible; no stale dark areas in Light; blue/orange pausing words readable; dashboard and tray menu match saved appearance | Pending |
| 100/150/200% DPI, small window and monitor move | No scrollbars when content fits; Advanced scroll and button hover do not flash; overflow and keyboard focus stay usable; captions fit | Pending |
| Tabs, Tab/Shift+Tab, arrow keys, F1, tooltips, screen reader | Native navigation/roles/text remain available; core understanding needs no hovering | Pending |
| Advanced persistence and tray parity | Default off; saved on reopen/restart; hidden tools inaccessible; matching core actions | Pending |
| Refresh, picker cancel, settings failure and repeated actions | Acknowledgement/completion/failure stays visible; duplicate work coalesces; no false saved state | Pending |
| Remove selected custom game among two custom/one launcher | Named Cancel-default confirmation; only saved selected custom entry removed | Pending |
| Automatic and manual Pause/Resume | No redundant Pause or hidden hold; busy actions disabled and revalidated | Pending |
| Gameplay Resume cancel/accept and separate Ignore choices | Cancel does nothing; accepted listed instances restore immediately; exclusions save independently | Pending |
| New game/relaunch during dialog or model load; app restart | Approval revoked, remaining loads guarded, original recovery retained | Pending |
| Pending provider disable/endpoint edit | Unfinished provider blocked with recovery action; completed provider independently editable | Pending |
| Single sound, rapid pause/restore, fullscreen and Windows suppression | At most one sound source; stale success discarded; persistent truthful state remains | Pending |
| Normal game exit and tray state | Full grace then verified restore; tray and dashboard agree | Pending |

## Live recovery and lifecycle

Live model/game, power, startup and installer experiments require separate authorization. Begin with a private data directory and observation; never overwrite a real pending recovery journal. Preserve a before/after inventory and complete settings/server evidence locally, with sanitized results in the review record.

- LM capture/unload/restore must preserve every loaded LLM/embedding instance, exact variant/identifier, TTL, raw/native settings and original server running state/port. Test initial server stopped and running. Unknown or partial evidence must retain recovery.
- Test a new game between loads, unavailable provider, partial model failure, app/provider restart and failed persistence without losing original snapshots or healthy-provider completion.
- Actual suspend/resume while gaming and while restoring must refresh evidence before later control, retain manual holds and start full grace after reliable empty-game evidence. Confirm coexistence only for the original live instances.
- Check supported 0.3.x settings and pending schema-2 migration using copied isolated data. Backups retain original bytes; unsupported files stay intact. Complete recovery before any downgrade.
- After separate authorization, check per-user install/upgrade/uninstall, portable execution, data retention, existing installer identity/path, startup registration and actual reboot/sign-in. Uninstall never performs recovery.
- Ollama live testing is optional for stable LM release acceptance. Contributor evidence must identify versions and verify only the documented subset; no download/cloud, automatic enrollment, service ownership or full-settings promise.

## Performance

Record repeated paired baseline/candidate samples with CPU normalization, working/private memory, wakeups, child-process costs, binary size and process-scan gaps. Cover dashboard closed/open, no/one/two private fixture providers, unavailable endpoint, simulated gaming and restoration. Keep actual provider/game runs separately authorized. Report sample duration/range and environment; historical preview figures are not candidate measurements or universal budgets.

Automated fixture success, compilation, physical appearance, human comprehension, real-provider compatibility and installation acceptance are separate evidence. The release remains blocked while required review/feedback or safety fixes are outstanding.
