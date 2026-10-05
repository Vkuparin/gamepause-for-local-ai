#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif

[Setup]
AppId={{4E394232-957B-4E05-B0EC-F81F31DBFC2D}
AppName=GamePause for LM Studio
AppVersion={#AppVersion}
AppPublisher=GamePause contributors
AppPublisherURL=https://github.com/Vkuparin/gamepause-lmstudio
AppSupportURL=https://github.com/Vkuparin/gamepause-lmstudio/issues
AppUpdatesURL=https://github.com/Vkuparin/gamepause-lmstudio/releases
DefaultDirName={localappdata}\Programs\GamePause
DefaultGroupName=GamePause for LM Studio
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
Name: "startup"; Description: "Start GamePause automatically when I sign in to Windows"; GroupDescription: "Startup:"
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "..\dist\GamePause\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\GamePause"; Filename: "{app}\GamePause.exe"
Name: "{group}\Usage guide"; Filename: "{app}\docs\USAGE.md"
Name: "{group}\Uninstall GamePause"; Filename: "{uninstallexe}"
Name: "{userdesktop}\GamePause"; Filename: "{app}\GamePause.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "GamePause"; ValueData: """{app}\GamePause.exe"" --background"; Tasks: startup; Flags: uninsdeletevalue

[Run]
Filename: "{app}\GamePause.exe"; Description: "Launch GamePause (automatic pausing is enabled)"; Flags: nowait postinstall skipifsilent
