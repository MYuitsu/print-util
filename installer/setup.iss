; Inno Setup 6 script for print-util
; Download Inno Setup: https://jrsoftware.org/isdl.php
;
; Build command (from repo root, after `cargo build --release`):
;   iscc installer\setup.iss
;
; Output: installer\Output\print-util-0.3.0-setup.exe

#include "store-config.iss"

#define AppName      "print-util"
#define AppVersion   "0.3.0"
#define AppPublisher StorePublisher
#define AppURL       StoreProjectURL
#define AppExe       "print-util.exe"
#define TrayExe      "print-util-tray.exe"
#define ServiceName  "print-util"
#define ServicePort  "17474"

[Setup]
AppId={{B4F1A2C3-1234-4567-ABCD-0123456789EF}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#StoreSupportURL}
AppUpdatesURL={#AppURL}/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
OutputDir=Output
OutputBaseFilename=print-util-{#AppVersion}-setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Windows 8+ (6.2) to keep compatibility for Win8/Win10 deployments
MinVersion=6.2
PrivilegesRequired=admin

#ifdef STORE_SIGNING
SignTool=store
SignedUninstaller=yes
#endif

; allow silent install: setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
; Main binary
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\{#TrayExe}"; DestDir: "{app}"; Flags: ignoreversion

; Bundled Ghostscript engines (required for offline printing fallback)
Source: "vendor\gsdll64.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "vendor\gswin64c.exe"; DestDir: "{app}"; Flags: ignoreversion
; GS resource files (gs_init.ps, fonts, etc.) required by gswin64c.exe
Source: "vendor\gs_lib\*"; DestDir: "{app}\gs_lib"; Flags: ignoreversion recursesubdirs

; SumatraPDF portable (primary print engine – required, single self-contained exe)
Source: "vendor\SumatraPDF.exe"; DestDir: "{app}"; Flags: ignoreversion

Source: "vendor\vnpt.ico"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{commonstartup}\VNPT Print Util"; Filename: "{app}\{#TrayExe}"; WorkingDir: "{app}"

[Run]
; Register and start Windows service after install
Filename: "{sys}\sc.exe"; \
  Parameters: "create ""{#ServiceName}"" binPath= ""{app}\{#AppExe} {#ServicePort}"" start= auto DisplayName= ""Print Util - Silent Print Server"""; \
  Flags: runhidden waituntilterminated; StatusMsg: "Registering service..."

Filename: "{sys}\sc.exe"; \
  Parameters: "description ""{#ServiceName}"" ""Silent PDF print server (print-util). Listens on 127.0.0.1:{#ServicePort}."""; \
  Flags: runhidden waituntilterminated

Filename: "{sys}\sc.exe"; \
  Parameters: "start ""{#ServiceName}"""; \
  Flags: runhidden waituntilterminated; StatusMsg: "Starting service..."

Filename: "{app}\{#TrayExe}"; \
  Description: "Run VNPT tray icon"; \
  Flags: nowait postinstall skipifsilent

[UninstallRun]
; Stop and remove service on uninstall
Filename: "{sys}\sc.exe"; Parameters: "stop ""{#ServiceName}""";  Flags: runhidden waituntilterminated; RunOnceId: "StopPrintUtilService"
Filename: "{sys}\sc.exe"; Parameters: "delete ""{#ServiceName}"""; Flags: runhidden waituntilterminated; RunOnceId: "DeletePrintUtilService"

[Code]
// On upgrade: stop and delete old service so sc.exe create in [Run] succeeds.
// Errors are intentionally ignored (service may not exist on first install).
procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
begin
  if CurStep = ssInstall then
  begin
    Exec(ExpandConstant('{sys}\sc.exe'), 'stop "{#ServiceName}"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Sleep(2000);
    Exec(ExpandConstant('{sys}\sc.exe'), 'delete "{#ServiceName}"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Sleep(500);
  end;
end;
