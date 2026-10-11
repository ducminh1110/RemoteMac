; Taro for Windows: the installer (Inno Setup 6). For this user only: no administrator, no
; UAC prompt, into %LOCALAPPDATA%\Programs\Taro; a Start-menu entry "Taro" (beside the folder
; of Mac apps Taro keeps while connected), an optional desktop icon, an uninstaller.
;
; Taro's own updates run it silently (update.rs): /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
; /CLOSEAPPLICATIONS /RELAUNCH=1, and it starts Taro again once it is done.
;
; Built by .github/workflows/release.yml:
;   ISCC /DAppVersion=1.2.3 /DNumericVersion=1.2.3.0 /DSourceDir=...\dist\Taro /DIconFile=...\dist\taro.ico taro.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\dist\Taro"
#endif
#ifndef IconFile
  #define IconFile "..\dist\taro.ico"
#endif

[Setup]
AppId={{8C1E7B42-6A5D-4F0B-9B7E-3D1A2C4F5E60}
AppName=Taro
AppVersion={#AppVersion}
AppVerName=Taro {#AppVersion}
AppPublisher=Taro
AppPublisherURL=https://github.com/ducminh1110/RemoteMac
AppSupportURL=https://github.com/ducminh1110/RemoteMac/issues
AppUpdatesURL=https://github.com/ducminh1110/RemoteMac/releases
VersionInfoVersion={#NumericVersion}
DefaultDirName={localappdata}\Programs\Taro
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
OutputBaseFilename=Taro-Setup
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\taro.ico
UninstallDisplayName=Taro
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; an update closes the Taro that runs (it closes itself first when it updates itself)
CloseApplications=force
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\Taro.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#IconFile}"; DestDir: "{app}"; DestName: "taro.ico"; Flags: ignoreversion

[Icons]
; "Taro.lnk" in the Start menu's Programs, not in its "Taro" folder: that one holds the Mac's
; apps while Taro is connected, and is emptied when it closes
Name: "{autoprograms}\Taro"; Filename: "{app}\Taro.exe"; IconFilename: "{app}\taro.ico"
Name: "{autodesktop}\Taro"; Filename: "{app}\Taro.exe"; IconFilename: "{app}\taro.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\Taro.exe"; Description: "{cm:LaunchProgram,Taro}"; Flags: nowait postinstall skipifsilent
; an update (silent): Taro again, as it was before
Filename: "{app}\Taro.exe"; Flags: nowait; Check: Relaunch

[Code]
function Relaunch: Boolean;
begin
  Result := ExpandConstant('{param:RELAUNCH|0}') = '1';
end;
