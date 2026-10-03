# Configuration

Default location: `%LOCALAPPDATA%\GamePause\config.json`. All changes require restart.
The complete starting configuration is [config.example.json](../config.example.json).

| Setting | Default | Meaning |
|---|---|---|
| `mode` | `observe` | `observe` detects only; `active` changes model/server state |
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
| `extra_games` | `[]` | Explicit `{ "name": "...", "path": "..." }` game installation directories |
| `excluded_executables` | `[]` | Additional executable names/globs (`*`, `?`), case-insensitive |
| `excluded_paths` | `[]` | Directories to exclude from gaming detection |

CLI `--active` and `--observe` override `mode` for that run without modifying config.
Other switches: `--headless`, `--duration N`, `--discover`, `--doctor`, `--restore`,
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

`path` means the game directory, **not** its executable. Avoid broad roots such as `C:\\`,
`Program Files`, or a folder containing unrelated applications. `game_roots` treats each
child directory as a game; installed folder contents are not recursively scanned.

Launcher clients, common installers, crash reporters, Lossless Scaling, Wallpaper Engine,
and known anti-cheat services are excluded by default. Unusual helpers need explicit exclusions.
Unknown configuration keys are errors so a misspelling doesn't silently change behaviour.
