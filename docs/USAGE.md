# Usage and recovery

## Everyday use

Keep LM Studio open and load whichever models you want available after gaming. Install GamePause and launch your games normally. Automatic pausing is enabled by default; there is no observation-mode setup or model-name list.

GamePause discovers Steam, Epic, EA, Ubisoft Connect, Battle.net, and Xbox installations automatically. Small launcher inventories refresh every 30 seconds. A new unfamiliar process in a known Steam library requests an early metadata refresh. It does not classify arbitrary executables as games just because they are in that library. Xbox package discovery refreshes every five minutes while idle; expensive refreshes defer during gaming.

When a recognized game starts, GamePause captures all loaded models and their settings before unloading. It uses the existing local server's port, or temporarily starts the server to capture settings and closes it again. The original server state is remembered. Known launchers, background utilities, installers, crash reporters, and anti-cheat helpers do not start gaming sessions.

Additional games share the original snapshot. Alt-tabbing keeps the session active. After the last recognized game exits, the default 30-second delay starts. A new game cancels the countdown. Restoration checks the saved model identities and settings and returns the server to its original running/stopped state. If nothing was loaded, nothing is loaded afterward.

## Dashboard

Left-click the tray icon or open GamePause from Start. Closing the window leaves monitoring running in the tray. Right-click the tray icon for the current state and the quick controls: the automatic-pausing toggle, **Pause AI**, **Resume AI**, **Open GamePause** and **Quit**. Everything else is in the dashboard. **Quit** stops the watcher and keeps pending recovery.

The status card at the top always shows one of four states, each with its own color:

| State | Color | Meaning |
|---|---|---|
| **AI RUNNING** | Blue | GamePause is not holding AI paused. It is watching for games, or automatic pausing is off. |
| **AI PAUSED** | Orange | AI is paused for a game, by you, or until the resume delay ends. |
| **LOADING** | Yellow | GamePause is checking games, pausing, resuming or testing. The main button shows **Loading...** until it can be used again. |
| **AI NEEDS ATTENTION** | Red | Game detection failed, a pause or resume did not finish, or saved AI is still waiting to be restored. The line below says what to do. |

The window icon and title bar accompany the state; title-bar tinting needs Windows 11. Below the state, the card names the running game and gives one short line for each enabled provider, such as **LM Studio: Models unloaded successfully**. **AI RUNNING** and **Ready** mean GamePause is not holding AI paused. GamePause does not ask LM Studio which models are loaded while idle, so they are not a statement about loaded models. **Models restored** describes the last completed resume. Activity holds detailed status, command results, failures and a bounded history of observed changes; clicking **Activity** again closes it. Worker logs remain in the data folder.

**Games**, **Running apps** and **Ignored** use searchable tables. Each game has a platform and an automatic-pause checkbox; click a column heading to sort by it, and again to reverse the order. A running game is named in the status card and listed under **Running apps**. Selecting a row shows its path and contextual actions; **More...** contains custom Rename/Remove and Copy path.

Each game shows the icon of its own executable. Icons are read from disk only while the dashboard is open, for the rows on screen. For a game folder, GamePause looks for the executable in that folder and at most two folder levels below it; a game whose executable cannot be found or read shows a generic icon until it is seen running. No artwork is downloaded. Launcher marks in the Platform column are GamePause's own drawings, not vendor logos. PID is shown only when the existing recognized-game evidence supplies it; the general application inventory does not collect PIDs.

The window supports resizing, maximization and Windows DPI scaling. Compact widths hide the secondary platform column and move the primary action beneath the status. Long titles and paths have tooltips. Scrollbars keep content reachable in small windows. Tab/Shift+Tab moves focus, Enter/Space activates controls, arrows move table selection, Escape dismisses dialogs and F1 opens keyboard help. In Advanced, Escape returns to the games; with unsaved edits it first asks whether to discard them. Gameplay Resume, removal and live verification require explicit confirmation; Cancel receives initial focus.

Choose **Appearance** in **Advanced > General** for **Follow Windows**, **Light**, or **Dark**. The saved preference also applies to the native tray menu. High contrast uses Windows colors. Successful command feedback disappears after five seconds; actionable failures remain visible and are recorded in Activity. Closing the window releases its renderer and leaves monitoring running in the tray. Reopening recreates the dashboard on its existing UI thread. Quit uses the existing watcher shutdown path and retains pending recovery.

- **Games:** search discovered installations, see the recognition source, and ignore or enable an entry. New recognized games are enabled automatically.
- **Running apps:** select a missed game and click **Add as game**. Accessible running applications are listed only while this page is open. Select the actual game executable, not a launcher. **More... > Ignore executable** stops that program file from triggering a pause, including one inside a launcher game's folder. An entry ignored this way with 2.0.0 had no effect on launcher games; remove it under **Ignored** and ignore the executable again.
- **Add game…:** browse for a standalone game's executable. Its full path is saved; the installation directory is not broadly classified.
- **More... > Remove:** removes only the selected game you added yourself. The confirmation names the game and defaults to Cancel. Launcher entries use Ignore instead. Failed saves leave the entry unchanged; removing a running game does not erase its pending recovery guard.
- **Ignored:** re-enable games or saved path exclusions. Built-in launcher/helper exclusions remain automatic.
- **Advanced > Detection > Refresh game list**: request discovery immediately. Feedback reports queued work, completion, no changes or errors after the discovery result arrives. Repeated requests share the pending refresh. Routine use does not require it.
- **Automatically pause AI while gaming:** takes effect without restarting and is remembered. Turning it off does not discard captured models: an existing session waits for recognized games to exit, then restores normally.
- **Advanced:** a saved page-visibility preference, initially off. Categories cover General, Detection, LM Studio, Ollama, Recovery and Diagnostics. Settings include connection/delay fields, launcher roots and exclusions, read-only diagnostics, Test round-trip, logs/status folder and Windows startup. These tools are dashboard-only; the tray menu is the same whether Advanced is shown or not. Back to games leaves automation, providers and recovery unchanged.
- **Quit:** stops the watcher and retains pending recovery. Closing the dashboard alone keeps monitoring running.

Selected rows show their path and why they are recognized. Discovery and save errors appear in the window. A missing LM Studio connection retains models/recovery and is retried automatically.

Action feedback stays visible until another action replaces it. Timer updates and older command results do not replace it with a generic hint or an old test report. Settings appear as saved only after persistence succeeds. Cancelled pickers leave settings unchanged and report cancellation. Folder-opening and Windows startup failures also appear in feedback.

## Manual actions

Under Advanced settings, **Windows notifications** controls visual completion/failure messages and **Sound for notifications** controls their sound. With visuals enabled, Windows provides the only sound. With visuals off and sound on, GamePause uses one standalone system sound. Turn both off for silence. Windows may suppress visual notifications or sounds; the dashboard and tray remain the persistent state record.

Verified success messages use an initial two-second delay on the existing tray timer; this does not delay pausing or restoration. Rapid restore replaces a queued pause message. New work, partial failure or an old message after a stalled UI discards stale success. A failed pause or restore, and saved AI found waiting after a restart, are announced at once and coalesced through retries until a healthy state returns. Game detection that is unavailable is announced only after it has stayed down for about 20 seconds, so waking the PC from sleep or starting GamePause raises no error notification. These timing values are initial acceptance choices and may be adjusted after desktop testing. Delivery is not guaranteed over fullscreen games or with notification suppression enabled.

Advanced pages share one draft: edit any fields, then **Save settings**, or **Discard edits**. Switches that apply at once (startup, notifications, sound, appearance, a game's rule) leave unsaved edits in place, as does a game-list refresh. Under **Advanced > Recovery**, save the restore delay and retry interval with **Save settings**. **Advanced > LM Studio** contains the loopback API address, CLI path with **Browse...**, control enrollment and captured-server preference. LM Studio enrollment and connection cannot change while its recovery is unfinished; restore that saved AI first. A completed provider can be disabled while another provider retains recovery. **Advanced > General** contains Windows sign-in startup, appearance and notification preferences. Diagnostics and Activity provide the logs folder; Activity can load up to 64 KiB of the current worker log on request; its lines start with the local date and time (lines written before 2.0.1 start with Unix seconds). The tray retains its corresponding advanced tools. Failed saves retain previous settings; stale advanced actions are rejected when Advanced is hidden.

**Pause AI** creates a manual hold. **Resume AI** releases that hold and immediately resumes captured AI when recovery is pending. If nothing was captured, it simply releases the hold. A completed automatic pause disables redundant Pause; clicking Pause again cannot create or release a hidden hold. Capture, unload, restore and test operations disable incompatible requests in both the dashboard and tray. The worker checks requests again before acting.

**Resume AI** removes the delay when recovery is pending, detection has succeeded and recognized games are absent. During gameplay, **Resume AI...** opens a warning: restoring models can compete with the game for VRAM. Cancel is the default. Confirming restores immediately and temporarily overrides automatic pausing for the listed running game instances. A new nonignored game, a game relaunch, any approved game exiting, unknown detection, **Pause AI**, or restarting GamePause ends that approval. Pause creates a manual hold. Failed loads retain recovery; **Retry resume** uses the same approval while it remains valid.

The warning also offers separate, initially unchecked **Ignore in future** choices. Only selected executable paths are saved. Saving exclusions and restoring AI have separate results: a failed preference save does not cancel your explicit restore choice. Ignoring a game alone never restores AI, and remembered games still guard ordinary recovery. CLI Restore and Test round-trip retain their normal game guards.

**Test round-trip** runs the capture, unload and restore cycle on purpose for every enabled AI app, LM Studio first and then Ollama, and reports each step. It requires confirmation and no running game or pending recovery.

The hint line under the state distinguishes a gaming pause, your own pause, the resume delay, waiting for a response to finish, and each kind of failure. A recovery journal alone does not mean AI has been unloaded, so unfinished recovery shows **AI NEEDS ATTENTION**, not **AI PAUSED**. Watching without a provider probe does not establish LM Studio availability. Partial failures retain recovery and never report a completed pause or restore.

The basic dashboard shows enabled providers separately, including pending recovery and errors. Advanced settings is not required to see a failure. The tray menu shows only the overall state, such as **AI needs attention** with a one-line hint; open the dashboard for provider detail. **Models restored** describes completed work; current residency is not polled afterward. Retry backoff values describe the last update. Automatic retries leave completed healthy-provider work alone. CLI `status.json` includes the same evidence under `provider_outcomes`.

Game detection continues while AI capture or loading is busy. If a recognized game starts during restoration, GamePause defers remaining loads after the current operation returns. A failed or timed-out fresh scan holds recovery. The dashboard can update detected games before the ongoing model operation finishes.

After Windows resumes, GamePause refreshes discovery and process caches before controlling AI. Old scans and dialog approvals cannot authorize recovery. If a provider operation was already running when power changed, it can finish, but the next operation is held. Original captured settings and unfinished recovery remain saved. When a fresh, reliable scan confirms no games are running, the full configured recovery delay starts again; sleep time does not finish that delay. Manual holds remain in effect.

A previous gameplay coexistence choice is held while discovery refreshes and is used again only after the same live process instances are verified. A new/relaunched game or failed/unknown detection revokes it. Restarting GamePause always revokes it. Observation mode leaves pending recovery bytes unchanged after resume. These rules have fixture and injected native-event coverage; actual computer sleep/wake and live provider restarts remain untested.

## Command-line switches

GamePause is used through its window and tray menu. `GamePause.exe` accepts a few switches:

| Switch | Effect |
|---|---|
| `--background` | Start in the tray without opening the window. Used by the Windows sign-in entry. |
| `--data-dir DIRECTORY` | Keep settings, recovery and logs in another folder. |
| `--restore` | With GamePause not running and no game open, retry a pending restore once and show the result in a message box. |

`status.json` in the data folder holds the last reported state, including `provider_outcomes`, for bug reports. There is no separate command-line program in the download; the `--status` and `--games` scripting commands of 2.0 were removed in 2.1.0. Developers can still build a console variant for tests and diagnostics, see [Development](DEVELOPMENT.md).

## Games GamePause does not know

A game from a launcher GamePause cannot read never triggers a pause on its own. To help you catch those, GamePause notices a program that stays fullscreen and in front for about ten seconds and is not a known game, a Windows component or a common fullscreen program such as a browser or video player. It then shows a notification and a prompt in the dashboard: **Add as game** registers it so AI pauses the next time it runs, and **Not a game** stops the suggestion for that program for good. AI is never paused on a suggestion alone. The check reads only the foreground window's size and owner. Turn it off under Advanced > Detection.

## Other AI apps

For local AI servers without their own integration, such as llama.cpp's `llama-server.exe` or KoboldCpp, GamePause can stop the program for gaming and start it again afterwards. This has not been tested with those tools themselves; the process handling was tested with stand-in programs. Under Advanced > Other apps, choose **Add app...** and pick the program file. Each app has a **Start again after gaming** switch.

- When a game starts, every running copy of that exact file is asked to close and, if it is still running after three seconds, ended. A console program such as `llama-server.exe` has no window to ask, so it is ended at once. Work in progress is interrupted; GamePause cannot wait for a generation to finish.
- After the game, each instance that was running is started again with the same command line and working folder, minimised. Environment variables set by a launcher or script are not kept, and a program started by a supervisor that restarts it on its own will fight the pause.
- If GamePause cannot read how a program was started (for example, it runs as administrator), it leaves that program running and reports the reason instead of stopping something it could not bring back.
- The start command is saved encrypted for your Windows account, because it can contain an API key. It is never shown or logged. If the journal is moved to another account or PC it cannot be read, and the app is reported as stopped and not restarted.
- With the switch off, the app is only stopped, and the status names it as stopped without relaunch.

**Test round-trip** does not cover these apps.

A system-wide shortcut can pause and resume AI without opening GamePause, including from inside a game. None is set by default: enter one, for example `Ctrl+Alt+P`, under Advanced > General and save. The shortcut pauses AI when pausing is available and otherwise resumes through the same guarded path as **Resume AI**, so a running game still asks for confirmation. If another program already owns the combination, GamePause says so and registers nothing.

Each game has one of three rules. **On** pauses AI automatically, **Off** ignores the game, and **Ask** leaves AI running and asks: when the game starts, a notification and a prompt in the dashboard offer **Pause AI for this game**. If you do not answer, nothing is paused. If you accept, AI is paused as for any other game and restored when the game exits; the next launch asks again. Set the rule from a selected game's **More...** menu (**Ask before pausing** / **Pause automatically**); the row's checkbox shows On, Off or Ask.

An AI app that is not there is not an error, and the status card lists only the AI apps this PC has. LM Studio appears when it is installed and Ollama when it is installed or running; an app you do not use gets no line. A listed app that is closed reads **Not running**, and **Ready** means its program is running and GamePause will pause it when a game starts. Whether an app is running is read from the list of running processes GamePause already keeps; the app itself is not contacted while idle. An Ollama that runs outside Windows, for example in WSL or a container, is therefore listed only while a pause is holding its models. While AI is paused, the status, the dashboard and the pause notification say roughly how much memory was freed, for example **about 17.3 GB freed**. Ollama reports the memory its models occupy; LM Studio reports model file sizes, so the figure is approximate, and it is not shown after GamePause restarts mid-pause. When a game starts and no models were loaded anywhere, the status says nothing was paused, and no success notification is sent for the pause or the later restore.

**Test round-trip** unloads/reloads all current models through a durable schema-3 journal containing the original LM snapshot. Close games and finish inference first. It rejects observation mode, discovery errors and existing recovery. A temporary server is used if needed, then returned to its original state after successful recovery. A game appearing between steps defers remaining loads. Failure/cancellation exits nonzero; unresolved models stay recoverable. The GUI action asks for confirmation and is unavailable during pause/recovery. `verify-unloaded` means an empty model inventory, not a stopped server.

**Read-only diagnostics** in Advanced settings probes enabled providers on the control worker. The scrollable provider details show the saved endpoint, guarantee, recovery state/action and cached probe timestamp. Repeat diagnostics after provider changes. Changing the saved provider connection marks earlier evidence stale. Disabled providers are not probed. Test round-trip remains a separate, confirmed operation.

The probes are independent and report endpoints and observation timestamps. Ollama version and resident inventory are probed separately; their success does not establish recovery compatibility or live-tested support. A stopped LM server or no loaded model leaves its WS compatibility probe unknown. Doctor never starts/stops services or loads/unloads models. Reports/logs and a writability probe are local filesystem writes; settings and recovery journals are left intact.

`status.json` carries `provider_evidence`-style outcomes from the running app, which can differ from the current service state. A diagnostics probe describes the time of the probe, not a continuous availability check.

## Settings migration

Settings now use version 4. Version-3 settings migrate on startup with their original bytes kept as `config.v3.backup.json`; every saved choice is kept, except that the Ollama entry, off by default in version 3, is enabled once. For version-2 and unversioned settings, GamePause validates them and maps the LM address, CLI path and server-stop choice into the `lmstudio-main` provider entry. Games, exclusions, delays and explicit automation choices remain. Version-2 observation mode remains observation; unversioned settings keep the existing one-time migration to active mode. The original file is saved as `config.v2.backup.json` before atomic replacement. Invalid settings, backup conflicts or save failures leave the source intact and report an error. Doctor reads older settings without migrating them.

The [example settings](../config.example.json) show the format. LM Studio and Ollama are both enabled by default. IDs are stable names, and endpoints must be local host/port pairs. While a provider's recovery is unfinished, settings cannot disable/remove it or reassign its ID/endpoint. If a manual file edit disables pending Ollama, its recovery stays pending while healthy LM recovery can finish; re-enable the original entry before retrying. Unsupported or reassigned unfinished bindings are refused and retained.

Older builds cannot read version-4 settings; 1.x builds need the `config.v3.backup.json` copy and builds before 1.0 the `config.v2.backup.json` copy. Before downgrading, restore pending AI with this build, quit GamePause, and keep a copy of the current settings. Then restore the original backup as `config.json` for the older build. The backup reflects migration time; carry later compatible game/preferences changes over deliberately. Never restore a backup over a running instance or use it to bypass pending recovery. Startup registration and data-directory paths are unchanged.

Advanced visibility, visual notifications and sound preferences are persisted. Notification controls are under Advanced; turning them off does not hide errors or change AI control/recovery.

Recovery journals now use schema 3. Active startup validates a supported schema-2 LM journal, saves its exact original bytes as `state.v2.backup.json`, then atomically replaces it. Model identities, raw settings, stages, server lifecycle and remembered games survive migration. Observation mode and Doctor leave the journal unchanged. Invalid or unknown payloads, changed provider bindings, conflicting backups and failed writes retain the authoritative journal and refuse control.

Older builds cannot recover schema-3 journals. Complete pending recovery with this build before downgrading. Never put the migration backup back over a later journal: it records an earlier transaction state. Backups are not used for automatic recovery. This build retains typed LM and Ollama recovery obligations together. Completed entries survive restart and retire atomically before a new pause when their provider is disabled or reassigned. Unfinished original snapshots remain authoritative.

## Background AI applications

Ollama is a regular provider and is on by default: if Ollama is running when a game starts, GamePause pauses it, and if nothing answers at its address GamePause reports **Not running** and leaves it alone, with no error. It was tested with Ollama 0.35.1 on one Windows PC with one local GGUF model; other versions are expected to work but are unverified. Under **Advanced > Ollama**, **Pause Ollama models while gaming** turns it off or on and the loopback endpoint can be changed; both take effect with **Save settings**. Settings saved by a 1.x build, where Ollama was off by default, are switched on once when they migrate; turning it off afterwards is kept. An Ollama that stops answering after its models were unloaded is a different case: the saved models stay pending and GamePause retries until Ollama is back.

When a game starts, GamePause unloads every local model Ollama has loaded. After gaming it reloads the local GGUF models that advertise `completion` (including tool, thinking and vision models) with the same identity/digest and observed context. The keep-alive timer is frozen during the pause: a model with four of its five minutes left gets four minutes again after the game, however long you played, and a model loaded with an indefinite keep-alive is reloaded as indefinite. A model whose keep-alive had already run out when the pause began is not reloaded. Other local models, such as embedding models or ones with unknown format, context or expiry, are unloaded and not reloaded; the provider status names them. Cloud or remote-backed models, and models whose catalog entry changed while loaded, stop the Ollama pause before anything is unloaded.

Unloading waits for running inference: Ollama acknowledges the request at once but keeps the model until the current generation finishes, and GamePause reports the pause as waiting until the model is gone. A reload may take up to five minutes per model before GamePause treats it as failed and retries. Restoration does not preserve all live load options, parallelism, conversations or KV cache. The user-owned service stays running; GamePause never downloads models or fights later client reloads. Tag changes or final-set eviction retain recovery. Journals written by earlier builds keep their original rule, where time spent paused consumed the keep-alive.

**Test round-trip** includes Ollama: after the LM Studio steps, `ollama-unload` unloads what is loaded and confirms it is gone, and `ollama-restore` reloads the restorable models and checks identity, context and residency. Ollama not running, or nothing loaded, is reported as nothing to test and does not fail the run. The live checks cover one Ollama version and model; embedding, vision and multi-model sessions have fixture coverage only. [Contributions](../CONTRIBUTING.md) with fixes and sanitized live evidence are welcome; the Advanced contribution button opens the same project guidance in a browser.

Clients using LM Studio's HTTP server are unavailable during gaming. Pause agents that independently restart the server or explicitly load models. With `stop_server_during_gaming: false`, a client using JIT loading can reload models immediately. GamePause does not prevent every later model load.

Busy inference defers capture and unloading until idle. Continuous background inference can therefore delay VRAM release.

If another client loads a new model during pausing, GamePause keeps the original recovery snapshot and reports incomplete protection. It does not replace that snapshot or unload the newly added model. Pause the client that is loading models, then retry or allow ordinary recovery after gaming. A completed pause does not continually query for later client reloads.

## Recovery

Only one cooperating GamePause instance may control a provider route, even when instances use different data directories. A conflict refuses capture/control and reports that another instance owns the route. Quit that instance or use its data directory, especially if it has pending recovery. LM Studio also has one service-wide claim because its CLI controls the service independently of the configured port. Observation and read-only diagnostics do not claim control. These claims cannot prevent other applications or older GamePause builds from changing models.

The durable journal is `%LOCALAPPDATA%\GamePause\state.json`. It records intention before model/server changes and retains the session's game paths even if an entry is removed or ignored. Relaunching waits for discovery before resuming an existing session, including when automatic pausing has been turned off; the models are restored after recognized games exit. Observation (`--observe`) is an advanced diagnostic override and never controls models or executes recovery.

If LM Studio was closed, reopen it. GamePause retries without requiring a restart. Missing models, incompatible protocols, or changed load settings keep recovery pending and show an error. Never delete the journal just to clear an error.

If GamePause will not stay open and a restore is still pending, retry it once from a terminal with GamePause quit and no game running. A message box reports the result:

```powershell
& "$env:LOCALAPPDATA\Programs\GamePause\GamePause.exe" --restore
```

For a custom `--data-dir`, pass the same directory. Restore before uninstalling or deleting model/configuration files.
