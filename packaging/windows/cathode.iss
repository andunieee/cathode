; Built by the release workflow: iscc /DVersion=0.3.0 /DBundle=<bundle dir> /DOutput=<dir> cathode.iss
[Setup]
AppId={{6D0C2B6E-3C5B-4E57-9B0A-CA7D0DE00001}
AppName=Cathode
AppVersion={#Version}
AppPublisher=andunie
DefaultDirName={autopf}\Cathode
DefaultGroupName=Cathode
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#Output}
OutputBaseFilename=cathode-windows-x64-setup-v{#Version}
SetupIconFile=cathode.ico
UninstallDisplayIcon={app}\bin\cathode.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Files]
Source: "{#Bundle}\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs ignoreversion

[Icons]
Name: "{group}\Cathode"; Filename: "{app}\bin\cathode.exe"
Name: "{autodesktop}\Cathode"; Filename: "{app}\bin\cathode.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

; cathode:// links, from the browser extension and the userscript
[Registry]
Root: HKA; Subkey: "Software\Classes\cathode"; ValueType: string; ValueData: "URL:Cathode"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\cathode"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\cathode\shell\open\command"; ValueType: string; ValueData: """{app}\bin\cathode.exe"" ""%1"""

[Run]
Filename: "{app}\bin\cathode.exe"; Description: "Launch Cathode"; Flags: nowait postinstall skipifsilent
