# Configuration

Normal use requires no file edits. Use the dashboard for automatic pausing, game registration/exclusions, startup, restore delay, and connection settings; those changes apply immediately. Advanced configuration lives in `%LOCALAPPDATA%\GamePause\config.json`; direct file edits require restarting the watcher.
The complete starting configuration is [config.example.json](../config.example.json).

| Setting | Default | Meaning |
|---|---|---|
| `mode` | `active` | Advanced diagnostic mode; ordinary users use the automatic-pausing switch |
| `automation_enabled` | `true` | Automatically pause for games; existing recovery still completes when off |
| `settings_version` | `3` | Configuration format/migration marker; do not edit |
| `ignored_games` | `[]` | Installation/executable paths disabled through the dashboard |
| `poll_seconds` | `2` | Process check interval, minimum 0.5 seconds |
| `discovery_seconds` | `30` | Local inventory refresh, minimum 10 seconds |
| `restore_delay_seconds` | `30` | Grace period after the last detected game exits; 0 disables delay |
| `retry_seconds` | `30` | Independent provider retry backoff, minimum 5 seconds |
| `providers` | LM Studio and Ollama enabled | Typed provider entries; see below |
| `ollama_default_applied` | `true` | Internal marker: Ollama's on-by-default has been applied to this file. Leave it as written |
| `advanced_settings_visible` | `false` | Show noncore settings/tools in dashboard and tray |
| `notifications_enabled` | `true` | Windows completion/failure notifications |
| `sound_enabled` | `true` | One sound source per event; independent of visuals |
| `steam_roots` | `[]` | Extra Steam client/library roots containing `steamapps` |
| `epic_manifest_dirs` | `[]` | Extra directories containing Epic `.item` manifests |
| `game_roots` | `[]` | Parent folders whose immediate subfolders are individual games |
| `extra_games` | `[]` | Explicit `{ "name": "...", "path": "..." }` game installation directories or exact executable paths |
| `excluded_executables` | `[]` | Additional executable names/globs (`*`, `?`), case-insensitive |
| `excluded_paths` | `[]` | Directories to exclude from gaming detection |

CLI `--active` and `--observe` override `mode` for that run without modifying config.
Other switches: `--background` (quiet tray startup), `--headless`, `--duration N`, `--discover`, `--doctor`, `--restore`,
`--data-dir DIRECTORY`, `--version`.

## Provider entries

Each entry has a stable `id`, a `kind` (`lmstudio` or `ollama`) and an `enabled` boolean. IDs, kinds and normalized endpoints must be unique. Endpoints accept only `localhost` or `127.0.0.1` with a valid port. Remote control is refused.

LM Studio stores `endpoint` (default `127.0.0.1:1234`), `lms_path` (empty for autodetection) and `stop_server_during_gaming` (default `true`) inside `connection`. Ollama stores `endpoint` (default `127.0.0.1:11434`) directly on its entry and is enabled by default. When nothing answers at that address, GamePause treats Ollama as not running and does nothing with it. Set `enabled` to `false` to turn it off; read the [restore subset and limits](USAGE.md#background-ai-applications). A settings file saved before Ollama became a regular provider has no `ollama_default_applied` field: GamePause enables its Ollama entry once at startup and writes the marker, so a later `false` is kept. GamePause does not start Ollama or download models.

Unfinished recovery blocks disabling/removing its provider or changing its ID/endpoint. Complete that provider's recovery first. A completed provider can be edited while another remains pending. Legacy flat LM fields are accepted only through guarded migration; new version-3 files must use provider entries. See [migration and downgrade](USAGE.md#settings-migration).

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

All timing values must be finite, respect their documented minimums, and be at most 86400 seconds (one day). Observation mode refuses manual restore and round-trip verification. Existing recovery is protected against game removal/exclusions for GUI, automatic and CLI restore. Visual notifications and sounds have separate saved preferences. With visuals enabled, Windows owns the sound; sound-only mode uses one system sound. Windows may suppress delivery.


`appearance` accepts `"system"` (default), `"light"` or `"dark"`. Existing version-3 settings without this field follow Windows. Advanced settings saves the preference for the dashboard and tray menu; high contrast takes precedence. The setting does not change provider control or recovery.
