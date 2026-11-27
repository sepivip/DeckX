[Setup]
AppId={{9F9911C5-8626-4A4A-A4E6-47EAC822FAB1}
AppName=DeckX
AppVersion={#DeckXVersion}
AppPublisher=DeckX contributors
AppPublisherURL=https://github.com/sepivip/DeckX
AppSupportURL=https://github.com/sepivip/DeckX/issues
AppUpdatesURL=https://github.com/sepivip/DeckX/releases
DefaultDirName={localappdata}\Programs\DeckX
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
CloseApplications=yes
CloseApplicationsFilter=deckx.exe
RestartApplications=no
UninstallDisplayName=DeckX
UninstallDisplayIcon={app}\deckx.exe
OutputBaseFilename=DeckX-{#DeckXVersion}-windows-x64-setup

[Files]
Source: "{#DeckXPayload}\deckx.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DeckXPayload}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DeckXPayload}\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DeckXPayload}\DEPENDENCY-LICENSES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#DeckXPayload}\assets\Inter-OFL.txt"; DestDir: "{app}\assets"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\DeckX"; Filename: "{app}\deckx.exe"; WorkingDir: "{app}"

[Run]
Filename: "{app}\deckx.exe"; Description: "Open DeckX"; Flags: postinstall nowait skipifsilent

[Code]
const
  StartupKey = 'Software\Microsoft\Windows\CurrentVersion\Run';

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  StartupCommand: String;
begin
  if (CurUninstallStep = usPostUninstall) and
     RegQueryStringValue(HKCU, StartupKey, 'DeckX', StartupCommand) and
     (CompareText(StartupCommand, '"' + ExpandConstant('{app}\deckx.exe') + '" --startup') = 0) then
    RegDeleteValue(HKCU, StartupKey, 'DeckX');
end;
