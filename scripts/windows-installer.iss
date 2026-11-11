#ifndef AppVersion
  #error AppVersion must be provided by package-windows.ps1
#endif
#ifndef PackageDir
  #error PackageDir must be provided by package-windows.ps1
#endif
#ifndef OutputDir
  #error OutputDir must be provided by package-windows.ps1
#endif

[Setup]
AppId={{E817B036-3198-4B03-A75D-C4793FA9C07A}
AppName=AndroidTool
AppVersion={#AppVersion}
AppPublisher=TouMingQAQ
AppPublisherURL=https://github.com/TouMingQAQ/AndroidTool
AppSupportURL=https://github.com/TouMingQAQ/AndroidTool/issues
DefaultDirName={localappdata}\Programs\AndroidTool
DefaultGroupName=AndroidTool
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=AndroidTool-v{#AppVersion}-windows-x64-setup
SetupIconFile=..\assets\androidtool.ico
UninstallDisplayIcon={app}\androidtool.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#PackageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\AndroidTool"; Filename: "{app}\androidtool.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\AndroidTool"; Filename: "{app}\androidtool.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\androidtool.exe"; Description: "{cm:LaunchProgram,AndroidTool}"; Flags: nowait postinstall skipifsilent
