# Usage and recovery

## Everyday use

Keep LM Studio open and load whichever models you want available after gaming. Install GamePause and launch your games normally. Automatic pausing is enabled by default; there is no observation-mode setup or model-name list.

GamePause discovers Steam, Epic, EA, Ubisoft Connect, Battle.net, and Xbox installations automatically. Small launcher inventories refresh every 30 seconds. A new unfamiliar process in a known Steam library requests an early metadata refresh. It does not classify arbitrary executables as games just because they are in that library. Xbox package discovery refreshes every five minutes while idle; expensive refreshes defer during gaming.

When a recognized game starts, GamePause captures all loaded models and their settings before unloading. It uses the existing local server's port, or temporarily starts the server to capture settings and closes it again. The original server state is remembered. Known launchers, background utilities, installers, crash reporters, and anti-cheat helpers do not start gaming sessions.

Additional games share the original snapshot. Alt-tabbing keeps the session active. After the last recognized game exits, the default 30-second delay starts. A new game cancels the countdown. Restoration checks the saved model identities and settings and returns the server to its original running/stopped state. If nothing was loaded, nothing is loaded afterward.

## Dashboard

Left-click the tray icon or open GamePause from Start. Closing the window leaves monitoring running in the tray. Right-click the tray icon for quick actions; **Quit** stops the watcher and keeps pending recovery.

- **Games:** search discovered installations, see running status and recognition source, and ignore or enable an entry. New recognized games are enabled automatically.
- **Running apps:** select a missed game and click **Add selected as game**. Accessible running applications are listed only while this page is open. Select the actual game executable, not a launcher.
- **Add game…:** browse for a standalone game's executable. Its full path is saved; the installation directory is not broadly classified.
- **Ignored:** re-enable games or saved path exclusions. Built-in launcher/helper exclusions remain automatic.
- **Refresh now:** request discovery immediately. Routine use does not require it.
- **Automatically pause AI while gaming:** takes effect without restarting and is remembered. Turning it off does not discard captured models: an existing session waits for recognized games to exit, then restores normally.
- **Start when I sign in to Windows:** controls per-user startup. Sign-in starts quietly in the tray.
- **Settings:** reveal optional **Restore after (seconds)** and **Local API** fields, saved with **Save settings**. **Locate lms…** handles unusual CLI installations. Normal launcher/LM Studio setups need no edits.
- **Test round-trip:** exercises the full pause/restore cycle without a game — captures loaded models, unloads them, confirms the server is empty, restores them, and compares the read-back settings field by field. The result appears in the feedback line, naming the failing step and field when something does not round-trip. Use it after upgrading LM Studio or after an odd restore.

Selected rows show their path and why they are recognized. Discovery and save errors appear in the window. A missing LM Studio connection retains models/recovery and is retried automatically.

## Manual actions

**Pause / resume AI manually** holds the session open without a game. Release it to allow restoration after the delay. **Restore AI now** removes the delay; recognized games still prevent restoring. **Test round-trip** (dashboard and tray) runs the whole capture → unload → restore cycle on purpose and reports each step; it is a test, not a session, and needs no game running. The tray menu exposes the same actions.

## CLI scripting

`GamePauseCLI.exe` exposes the diagnostics and read-only state as stable, one-line-per-item output for scripts and monitoring. These read the files a running instance already writes, so they work even while a GUI instance is active and never touch the models or server.

```powershell
# Current state as key=value lines (booleans as yes/no, missing fields as -)
.\GamePauseCLI.exe --status
# Installed games as name<TAB>launcher<TAB>path, one per line
.\GamePauseCLI.exe --games
# Full doctor report (LM Studio version, server state, models, data dir)
.\GamePauseCLI.exe --doctor
# Round-trip test: capture, unload, confirm empty, restore, compare fields
.\GamePauseCLI.exe --verify
```

If the app is not running, `--status` prints `status=absent` and `--games` prints `games=absent`; a corrupt state file is an explicit error, not silence. `--verify` needs a live LM Studio server and exits non-zero if any step fails.

## Background AI applications

Clients using LM Studio's HTTP server are unavailable during gaming. Pause agents that independently restart the server or explicitly load models. With `stop_server_during_gaming: false`, a client using JIT loading can reload models immediately. GamePause does not prevent every later model load.

Busy inference defers capture and unloading until idle. Continuous background inference can therefore delay VRAM release.

## Recovery

The durable journal is `%LOCALAPPDATA%\GamePause\state.json`. It records intention before model/server changes and retains the session's game paths even if an entry is removed or ignored. Relaunching waits for discovery before resuming an existing session, including when automatic pausing has been turned off; the models are restored after recognized games exit. Observation (`--observe`) is an advanced diagnostic override and never controls models or executes recovery.

If LM Studio was closed, reopen it. GamePause retries without requiring a restart. Missing models, incompatible protocols, or changed load settings keep recovery pending and show an error. Never delete the journal just to clear an error.

With GamePause quit and no game running, diagnostics can retry recovery:

```powershell
.\GamePauseCLI.exe --restore
```

For a custom `--data-dir`, use the same directory. Restore before uninstalling or deleting model/configuration files.
