# Installation

## Requirements

- Windows 10/11 x64 and LM Studio with its `lms` CLI installed.
- LM Studio open. GamePause handles the local HTTP server automatically and does not launch the LM Studio application itself.
- Your desired models loaded before gaming. Model names are captured dynamically.

Destination PCs do not need Python, Node.js, Rust, or administrator rights. The development tools below are needed only to build from source.

## Installer

Download the setup EXE and `SHA256SUMS.txt` from [the same release](https://github.com/Vkuparin/gamepause-lmstudio/releases). Compare the published hash with:

```powershell
Get-FileHash .\GamePause-1.0.0-Setup.exe -Algorithm SHA256
```

The default installation path is `%LOCALAPPDATA%\Programs\GamePause`. Setup creates Start menu shortcuts and a Windows Installed apps entry. **Start GamePause in the system tray when I sign in to Windows** is checked by default. The desktop shortcut is optional.

The last setup page offers **Start GamePause in the system tray**. It starts monitoring without opening a window: look for the GamePause icon in the notification area, which may be under the **^** overflow arrow. Left-click that icon, or use the Start menu or desktop shortcut, to open the dashboard.

Automatic pausing is enabled on first run. Launch games normally; open the dashboard to see discovery and session status. No mode switch or configuration edit is required. Configuration and recovery live separately in `%LOCALAPPDATA%\GamePause`.

## Automatic startup

Use the installer checkbox or tray **Start with Windows** option. They register a per-user `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value named `GamePause`. It starts after sign-in, not as a service before login. Windows Startup apps settings may also disable it.

The tray startup command includes a custom data directory when used. Keep the executable in a stable location. Startup uses `--background` to remain in the tray and remembers the automatic-pausing switch. Opening the Start menu shortcut shows the existing dashboard rather than starting a second watcher.

## Portable ZIP

Extract the whole ZIP into a permanent directory. Run `GamePause.exe` for the tray or `GamePauseCLI.exe` for diagnostics. Both are standalone native executables. Keep documentation and dependency licenses alongside them. Enable startup through the tray if desired.

## LM Studio configuration

GamePause searches `PATH` and standard CLI locations. GamePause stays running and retries if the CLI is unavailable. Install/bootstrap it through LM Studio; for an unusual installation, use **Locate lms…** under Advanced settings. In version-3 files, update `connection` inside the existing `lmstudio` provider entry, preserving its ID and enabled choice:

```json
"connection": {
  "endpoint": "127.0.0.1:1234",
  "lms_path": "D:\\Tools\\LMStudio\\bin\\lms.exe",
  "stop_server_during_gaming": true
}
```

An already-running server port is detected automatically. The optional **Local API** dashboard field selects the port used when GamePause opens a temporary server. Only `localhost` and `127.0.0.1` are accepted. If REST authentication is enabled, provide `GAMEPAUSE_LM_API_TOKEN` in the app's environment. Never share the token. Internal WebSocket compatibility is checked separately; a REST token does not guarantee internal control access.

## Upgrade and uninstall

Restore AI and quit GamePause before upgrading. Install into the same directory. Configuration and recovery persist in the separate data folder. An incompatible journal is retained and reported. Version-3 settings and recovery use guarded migrations with byte-preserving backups. Games, exclusions, delays, original LM settings/server state and explicit automation choices survive; unversioned settings retain the older one-time active-mode migration rule. Read [migration and downgrade](USAGE.md#settings-migration) before using an older build. Candidate upgrade/uninstall and reboot/sign-in acceptance remain pending.

Uninstall from Windows Installed apps or the Start menu shortcut. Application files and its startup entry are removed. `%LOCALAPPDATA%\GamePause` is deliberately preserved. Restore models before deleting that folder. Uninstall does not restore paused models.

## Source builds

Install Rust through rustup and Visual Studio C++ Build Tools with the Windows SDK. Clone this repository. `rust-toolchain.toml` pins the compiler and formatting/lint components.

```powershell
cargo build --locked --release
.\target\release\GamePause.exe
.\target\release\GamePauseCLI.exe --doctor
```

Installer builds also need Inno Setup 6; see [Development](DEVELOPMENT.md).
