# Troubleshooting

## Gather diagnostics

Read-only Doctor, Status and cached Games work alongside the watcher. Under Advanced settings, Read-only diagnostics shows the same independent provider evidence without a terminal. Quit the watcher before a second observation run, Restore or disruptive Verify. From the installed/portable folder:

```powershell
.\GamePauseCLI.exe --doctor
.\GamePauseCLI.exe --discover
.\GamePauseCLI.exe --observe --headless --duration 30
.\GamePauseCLI.exe --verify
```

Reports, `status.json`, `inventory.json`, and rotated `gamepause.log` files are in `%LOCALAPPDATA%\GamePause`. Use `--data-dir DIRECTORY` consistently for a custom installation. Doctor reports independent probe outcomes without changing LM Studio model/server state; unavailable CLI, busy inference, protocol errors, or unwritable storage do not discard all results. WS compatibility is unknown when the server is stopped or no model is loaded. `--verify` is disruptive: it persists recovery, unloads, confirms an empty inventory, restores and compares settings. Close the GUI/games and finish inference first. Existing/corrupt recovery blocks a new verify. Failed or deferred restoration remains in `state.json`; use Restore or `--restore` after games exit. The report uses `capture`, `unload`, `verify-unloaded`, `restore`, and `verify-fields`. Actual WS operation logs include the instance and `lm=cli:...`; this identifies the CLI version, not an independently measured server version. Review and redact reports before posting them.

## A game is missed

Check `inventory.json` after an inventory refresh. Newly installed launcher games normally appear within 30 seconds; Xbox package metadata can take five minutes when idle. Use **Advanced > Detection > Refresh game list** in the dashboard for a manual update. A game started before its installation metadata appears may be detected after the next refresh.

Open **Running apps**, select the actual game, and choose **Add selected as game**, or use **Add game…** to browse for its executable. No restart is needed. Protected processes cannot always be inspected without elevation; GamePause deliberately runs without administrator rights. The inaccessible-process count includes Windows services and does not mean all those processes are games.

## A helper is treated as a game

Select its game entry and click **Ignore selected**. Advanced exclusions also support executable names/globs and directories. Keep custom roots narrow. Built-in exclusions cover common launcher, installer, crash reporting, and anti-cheat helpers; unusual helpers need explicit exclusions.

## AI does not pause

Verify **Automatically pause AI while gaming** is checked and the game is enabled. Keep LM Studio open; its server is handled automatically. `--doctor` should report `snapshot_ok`. Busy inference/queued requests defer capture; retry occurs after the backoff interval. A capture failure leaves models loaded and reports the error.

If the CLI is missing, install/bootstrap it in LM Studio or use **Locate lms…**. GamePause stays running and retries. Native API or WebSocket failures can indicate a port, token, server, or LM Studio protocol compatibility problem. Upgrading LM Studio may require an updated GamePause version.

## AI does not restore

Wait until every recognized game process exits, then allow the configured 30-second delay and model load time. Manual pause can hold a session open. Check `last_error` in `status.json` and the log. Reopen LM Studio if you closed it.

Recovery checks exact model selection, identifiers, TTL policy, raw load fields, and native configuration. A mismatch preserves `state.json`; it does not silently accept different settings. Missing model files, changed settings, or a conflicting instance with the same identifier need resolution. Restore manually in LM Studio if necessary, comparing the saved journal. Do not delete the journal just to dismiss an error.

With GamePause quit and no game running, retry recovery using:

```powershell
.\GamePauseCLI.exe --restore
```

## Startup or multiple-instance problems

Enable **Start when I sign in to Windows** and check Windows Startup apps. The executable must still exist at the registered path. Opening GamePause again shows its existing dashboard; a second watcher using the same data directory is prevented. Quit the first instance before diagnostics/recovery or use a separate data directory for read-only experiments.

## Models reload during gaming

Keep LM's `providers[].connection.stop_server_during_gaming` enabled. Pause clients that independently restart the server, control models through other interfaces, or load models from LM Studio's UI. GamePause unloads captured instances; it does not prevent every later model load. Experimental Ollama leaves the user-owned service running and does not fight later client reloads.
