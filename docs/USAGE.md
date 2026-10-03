# Usage and recovery

## First session

Keep LM Studio open, with its server running and the models you want available after gaming.
Run GamePause in observation mode, launch a game, and check its detected name. Correct
exclusions or missing locations before switching to active mode. Restart after config edits.

In active mode, the first detected game creates a durable snapshot. GamePause stops the
HTTP server by default and unloads the original instances. Additional games share the same
snapshot. Alt-tabbing does not end a gaming session. A process exit or crash does.

When no recognized games remain, the 30-second timer starts. Starting another game cancels
that timer. Restoration reloads the saved models and checks their settings. The server is
returned to its original running state. If nothing was loaded initially, nothing is loaded
afterward.

## Background AI applications

Clients using LM Studio's HTTP server will be unavailable during gaming. Pause background
agents that can independently restart the server or explicitly load models. With
`stop_server_during_gaming: false`, GamePause only unloads the originals; a client using JIT
loading can immediately reload them. This setting is suitable only if clients are quiet.

## Manual controls

**Pause AI manually** holds the same session open even without a recognized game.
Untick it to release that hold. **Restore AI now** removes the exit delay, but recognized
games still prevent restoration. **Disable detection** freezes actions without deleting
recovery. You may restore manually while detection is disabled and no game remains.

## Recovery after a crash or quit

The recovery journal is `state.json`. It records intention before destructive transitions,
including each instance's unload/load stage. Relaunching in active mode resumes the same
session instead of overwriting the original snapshot. Observation mode never executes recovery.

With GamePause exited and no game running:

```powershell
.\GamePauseCLI.exe --restore
```

For source runs:

```powershell
.\target\release\GamePauseCLI.exe --restore
```

If you use `--data-dir`, supply the **same directory** to recovery. Another instance is blocked
from modifying that directory concurrently. Never delete `state.json` just to clear an error;
first inspect logs and restore the models manually if necessary.

GamePause doesn't automatically reopen LM Studio after you close it. Pending recovery stays
on disk. Reopen LM Studio and restart GamePause or run recovery when you want AI available again.
