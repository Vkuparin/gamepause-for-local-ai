#ifndef AppVersion
  #define AppVersion "2.5.0"
#endif

[Setup]
AppId={{4E394232-957B-4E05-B0EC-F81F31DBFC2D}
AppName=GamePause for Local AI
AppVersion={#AppVersion}
AppPublisher=GamePause contributors
AppPublisherURL=https://github.com/Vkuparin/gamepause-for-local-ai
AppSupportURL=https://github.com/Vkuparin/gamepause-for-local-ai/issues
AppUpdatesURL=https://github.com/Vkuparin/gamepause-for-local-ai/releases
DefaultDirName={localappdata}\Programs\GamePause
DefaultGroupName=GamePause for Local AI
; The group was renamed in 2.1.0; do not reuse the old name on upgrade.
UsePreviousGroup=no
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\dist
OutputBaseFilename=GamePause-{#AppVersion}-Setup
LicenseFile=..\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
SetupIconFile=..\assets\gamepause.ico
UninstallDisplayIcon={app}\GamePause.exe
CloseApplications=yes
RestartApplications=no
SetupLogging=yes

[Tasks]
Name: "startup"; Description: "Start GamePause in the system tray when I sign in to Windows"; GroupDescription: "Startup:"
Name: "desktopicon"; Description: "Create a desktop shortcut that opens the dashboard"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "..\dist\GamePause\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[InstallDelete]
; Shortcuts created under the pre-2.1.0 name.
Type: filesandordirs; Name: "{userprograms}\GamePause for LM Studio"
Type: files; Name: "{userdesktop}\GamePause.lnk"
; Files earlier versions installed that are no longer shipped.
Type: files; Name: "{app}\GamePauseCLI.exe"
Type: files; Name: "{app}\docs\ACCEPTANCE.md"
Type: files; Name: "{app}\docs\UI_V1.5.0_JOURNAL.md"

[Icons]
Name: "{group}\GamePause for Local AI"; Filename: "{app}\GamePause.exe"; Comment: "Open the GamePause dashboard"
Name: "{group}\Usage guide"; Filename: "{app}\docs\USAGE.md"
Name: "{group}\Uninstall GamePause"; Filename: "{uninstallexe}"
Name: "{userdesktop}\GamePause for Local AI"; Filename: "{app}\GamePause.exe"; Comment: "Open the GamePause dashboard"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "GamePause"; ValueData: """{app}\GamePause.exe"" --background"; Tasks: startup; Flags: uninsdeletevalue

[Run]
Filename: "{app}\GamePause.exe"; Parameters: "--background"; Description: "Start GamePause in the system tray (automatic pausing is enabled)"; Flags: nowait postinstall skipifsilent
