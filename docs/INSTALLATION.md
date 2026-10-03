# Installation

## Requirements

- Windows 10/11 x64 and LM Studio with its `lms` CLI installed.
- LM Studio open, with its local server running on the configured port (default `127.0.0.1:1234`). GamePause does not start LM Studio itself.
- Your desired models loaded before gaming. Model names are captured dynamically.

Destination PCs do not need Python, Node.js, Rust, or administrator rights. The development tools below are needed only to build from source.

## Installer

Download the setup EXE and `SHA256SUMS.txt` from [the same release](https://github.com/Vkuparin/gamepause-lmstudio/releases). Compare the published hash with:

```powershell
Get-FileHash .\GamePause-0.1.0-Setup.exe -Algorithm SHA256
```

The default installation path is `%LOCALAPPDATA%\Programs\GamePause`. Setup creates Start menu shortcuts and a Windows Installed apps entry. **Start GamePause automatically when I sign in to Windows** is checked by default. The desktop shortcut is optional.

First run uses observation mode. Follow [Usage](USAGE.md) before enabling active mode. Configuration and recovery live separately in `%LOCALAPPDATA%\GamePause`.

## Automatic startup

Use the installer checkbox or tray **Start with Windows** option. They register a per-user `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value named `GamePause`. It starts after sign-in, not as a service before login. Windows Startup apps settings may also disable it.

The tray startup command includes a custom data directory when used. Keep the executable in a stable location. Startup uses the saved mode; change `mode` to `active` once detection is verified.

## Portable ZIP

Extract the whole ZIP into a permanent directory. Run `GamePause.exe` for the tray or `GamePauseCLI.exe` for diagnostics. Both are standalone native executables. Keep documentation and dependency licenses alongside them. Enable startup through the tray if desired.

## LM Studio configuration

GamePause searches `PATH` and standard CLI locations. If `--doctor` cannot find `lms`, install/bootstrap the CLI through LM Studio or set `lms_path` to its absolute location:

```json
"lms_path": "D:\\Tools\\LMStudio\\bin\\lms.exe"
```

Set `api_host` for a different local port. Only `localhost` and `127.0.0.1` are accepted. If REST authentication is enabled, provide `GAMEPAUSE_LM_API_TOKEN` in the app's environment. Never share the token. Internal WebSocket compatibility is checked separately; a REST token does not guarantee internal control access.

## Upgrade and uninstall

Restore AI and quit GamePause before upgrading. Install into the same directory. Configuration and recovery persist in the separate data folder. An incompatible journal is retained and reported.

Uninstall from Windows Installed apps or the Start menu shortcut. Application files and its startup entry are removed. `%LOCALAPPDATA%\GamePause` is deliberately preserved. Restore models before deleting that folder. Uninstall does not restore paused models.

## Source builds

Install Rust through rustup and Visual Studio C++ Build Tools with the Windows SDK. Clone this repository. `rust-toolchain.toml` pins the compiler and formatting/lint components.

```powershell
cargo build --locked --release
.\target\release\GamePause.exe
.\target\release\GamePauseCLI.exe --doctor
```

Installer builds also need Inno Setup 6; see [Development](DEVELOPMENT.md).
