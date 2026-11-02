; The Windows installer, for Inno Setup 6. scripts/release-windows.ps1 runs
; it as ISCC /DAppVersion=<version> /DStage=<folder> /DOutputDir=<folder>,
; with the x64 and ARM64 builds in <Stage>\x64 and <Stage>\arm64 and the
; licence files in <Stage>.
;
; Vento installs for the current user only (no administrator rights), in
; %LOCALAPPDATA%\Programs\Vento, starts when the user signs in unless that
; task is unticked, and is set up in English or Arabic after Windows' own
; language. Uninstalling leaves the settings and the Pocket in
; %APPDATA%\vento.

#ifndef AppVersion
  #error Pass /DAppVersion=<version>
#endif
#ifndef Stage
  #error Pass /DStage=<folder>
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
; Never change: Windows knows an installed Vento by it.
AppId={{DDA25542-3329-4B66-BA87-E357FCFEE113}
AppName=Vento
AppVersion={#AppVersion}
AppVerName=Vento {#AppVersion}
AppPublisher=Fathi Alamre
AppPublisherURL=https://github.com/fathialamre/vento
AppSupportURL=https://github.com/fathialamre/vento/issues
AppUpdatesURL=https://github.com/fathialamre/vento/releases
VersionInfoVersion={#AppVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\Vento
DisableProgramGroupPage=yes
DisableDirPage=auto
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; A running Vento is asked to close (it hears the session end) before its
; files are replaced.
CloseApplications=force
RestartApplications=no
SetupIconFile=..\crates\vento\assets\vento.ico
UninstallDisplayIcon={app}\vento.exe
UninstallDisplayName=Vento
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename=vento-{#AppVersion}-windows-setup
ShowLanguageDialog=no
LanguageDetectionMethod=uilanguage

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ar"; MessagesFile: "compiler:Languages\Arabic.isl"

[CustomMessages]
en.AutoStart=Start Vento when I sign in to Windows
ar.AutoStart=تشغيل Vento عند تسجيل الدخول إلى Windows

[Tasks]
Name: "autostart"; Description: "{cm:AutoStart}"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Stage}\x64\vento.exe"; DestDir: "{app}"; Check: not IsArm64; Flags: ignoreversion
Source: "{#Stage}\arm64\vento.exe"; DestDir: "{app}"; Check: IsArm64; Flags: ignoreversion
Source: "{#Stage}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "{#Stage}\LICENSE-tabler"; DestDir: "{app}"; DestName: "LICENSE-tabler.txt"; Flags: ignoreversion
Source: "{#Stage}\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Vento"; Filename: "{app}\vento.exe"
Name: "{autodesktop}\Vento"; Filename: "{app}\vento.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Vento"; ValueData: """{app}\vento.exe"""; Flags: uninsdeletevalue; Tasks: autostart
; Unticked on an update: it no longer starts at sign-in.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Vento"; Flags: deletevalue; Tasks: not autostart

[Run]
Filename: "{app}\vento.exe"; Description: "{cm:LaunchProgram,Vento}"; Flags: nowait postinstall skipifsilent

[Code]
// Vento has no window to close, so the uninstaller ends it before
// removing its files.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  if CurUninstallStep = usUninstall then
    Exec(ExpandConstant('{sys}\taskkill.exe'), '/f /im vento.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;
