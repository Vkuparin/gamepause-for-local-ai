# Configuration

Normal use requires no file edits. Use the dashboard for automatic pausing, game registration/exclusions, startup, restore delay, and connection settings; those changes apply immediately. Advanced configuration lives in `%LOCALAPPDATA%\GamePause\config.json`; direct file edits require restarting the watcher.
The complete starting configuration is [config.example.json](../config.example.json).

| Setting | Default | Meaning |
|---|---|---|
| `mode` | `active` | Advanced diagnostic mode; ordinary users use the automatic-pausing switch |
| `automation_enabled` | `true` | Automatically pause for games; existing recovery still completes when off |
| `settings_version` | `2` | Configuration format/migration marker; do not edit |
| `ignored_games` | `[]` | Installation/executable paths disabled through the dashboard |
| `poll_seconds` | `2` | Process check interval, minimum 0.5 seconds |
| `discovery_seconds` | `30` | Local inventory refresh, minimum 10 seconds |
| `restore_delay_seconds` | `30` | Grace period after the last detected game exits; 0 disables delay |
| `retry_seconds` | `30` | Backoff after LM Studio failures, minimum 5 seconds |
| `stop_server_during_gaming` | `true` | Stop HTTP clients from JIT-loading models during gaming |
| `lms_path` | `""` | Optional full path to `lms.exe`; otherwise autodetect |
| `api_host` | `127.0.0.1:1234` | Local control/REST address; remote hosts are intentionally refused |
| `steam_roots` | `[]` | Extra Steam client/library roots containing `steamapps` |
| `epic_manifest_dirs` | `[]` | Extra directories containing Epic `.item` manifests |
| `game_roots` | `[]` | Parent folders whose immediate subfolders are individual games |
| `extra_games` | `[]` | Explicit `{ "name": "...", "path": "..." }` game installation directories or exact executable paths |
| `excluded_executables` | `[]` | Additional executable names/globs (`*`, `?`), case-insensitive |
| `excluded_paths` | `[]` | Directories to exclude from gaming detection |

CLI `--active` and `--observe` override `mode` for that run without modifying config.
Other switches: `--background` (quiet tray startup), `--headless`, `--duration N`, `--discover`, `--doctor`, `--restore`,
`--data-dir DIRECTORY`, `--version`.

## Standalone or missed installations

Example additions (merge into your existing configuration):

```json
{
  "game_roots": ["D:\\StandaloneGames"],
  "extra_games": [{"name": "My Game", "path": "E:\\SpecialLocation\\My Game"}],
  "excluded_executables": ["MyGameUpdater.exe", "*CrashReporter*.exe"],
  "excluded_paths": ["D:\\StandaloneGames\\Utilities"]
}
```

`path` accepts a game directory or an exact executable. The dashboard saves an exact executable for manually added games. Avoid broad roots such as `C:\\`,
`Program Files`, or a folder containing unrelated applications. `game_roots` treats each
child directory as a game; installed folder contents are not recursively scanned.

Launcher clients, common installers, crash reporters, Lossless Scaling, Wallpaper Engine,
and known anti-cheat services are excluded by default. Unusual helpers need explicit exclusions.
Unknown configuration keys are errors so a misspelling doesn't silently change behaviour.

All timing values must be finite, respect their documented minimums, and be at most 86400 seconds (one day). Observation mode refuses manual restore and round-trip verification. Existing recovery is protected against game removal/exclusions for GUI, automatic and CLI restore. Notification sounds currently follow completion/error events and are always enabled; there is no sound preference in this preview.
