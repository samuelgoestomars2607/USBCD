; USBCD – Windows-Installer (Inno Setup 6.6+, wegen Dunkelmodus)
; Nicht direkt kompilieren, sondern build-installer.ps1 ausführen:
; das Skript baut USBCD und lädt FFmpeg, adb und den Monitor-Treiber (geprüft) nach deps\.
;
; Aufbau nach der Installation:
;   USBCD.exe            läuft OHNE Administratorrechte (Autostart über HKCU\…\Run)
;   Dienst USBCDHelper   USBCD.exe --service als LocalSystem; nimmt nur feste Befehle an
;                        (Monitor ein/aus, Lautsprecher benennen)
;   usbcd_cam.dll        Webcam „USBCD Tablet Webcam“ (nur wenn im Menü eingeschaltet);
;                        lädt der Windows-Kameradienst über die CLSID unten

#if Ver < EncodeVer(6,6,0)
  #error Inno Setup 6.6 oder neuer nötig (Dunkelmodus) – build-installer.ps1 erneut ausführen, es aktualisiert Inno Setup
#endif

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#define AppName "USBCD"
#define HostExe "USBCD.exe"
#define OldTask "USBCD"
#ifdef Sign
  #define SignFlag " signonce"
#else
  #define SignFlag ""
#endif

[Setup]
AppId={{8C4E2B1A-6D3F-4A7E-9B21-5F0C3D7E9A41}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Samuel E. Ramirez
AppComments=Android tablet via USB-C as a second monitor with speakers
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.22000
OutputDir=Output
OutputBaseFilename=USBCD-Setup-{#AppVersion}
SetupIconFile=USBCD.ico
UninstallDisplayIcon={app}\USBCD.ico
UninstallDisplayName={#AppName}
Compression=lzma2/max
SolidCompression=yes
; Hell oder dunkel nach der Windows-Einstellung; eigene Bilder im USBCD-Design (wizard\)
WizardStyle=modern dynamic
WizardImageFile=wizard\hell\gross-*.png
WizardImageFileDynamicDark=wizard\dunkel\gross-*.png
WizardSmallImageFile=wizard\hell\klein-*.png
WizardSmallImageFileDynamicDark=wizard\dunkel\klein-*.png
; Sprachauswahl beim Start (vorausgewählt: Windows-Sprache)
ShowLanguageDialog=yes
LanguageDetectionMethod=uilanguage
CloseApplications=no
; Autostart-Eintrag liegt bewusst unter HKCU (USBCD läuft ohne Administratorrechte)
UsedUserAreasWarning=no
#ifdef Sign
; Code-Signatur (build-installer.ps1 mit USBCD_SIGN_CERT_THUMBPRINT)
SignTool=usbcd
SignedUninstaller=yes
#endif

[Languages]
; Erklärseite vor der Installation je Sprache
Name: "en"; MessagesFile: "compiler:Default.isl"; InfoBeforeFile: "before-install.txt"
Name: "de"; MessagesFile: "compiler:Languages\German.isl"; InfoBeforeFile: "vor-der-installation.txt"

[CustomMessages]
en.TaskVbCable=Install virtual speakers (VB-CABLE, downloaded from vb-audio.com – needed for sound)
de.TaskVbCable=Virtuellen Lautsprecher installieren (VB-CABLE, wird von vb-audio.com geladen – nötig für den Ton)
en.TaskAutostart=Start automatically in the background at sign-in (recommended)
de.TaskAutostart=Beim Anmelden automatisch im Hintergrund starten (empfohlen)
en.IconSettings=USBCD – Settings and log
de.IconSettings=USBCD – Einstellungen und Log
en.IconDiagnose=USBCD – Diagnosis
de.IconDiagnose=USBCD – Diagnose
en.IconComment=Start USBCD – icon in the notification area
de.IconComment=USBCD starten – Symbol im Infobereich der Taskleiste
en.VbDownloadFailed=VB-CABLE could not be downloaded.%nThe picture works anyway; for sound, run this setup again later.
de.VbDownloadFailed=VB-CABLE konnte nicht heruntergeladen werden.%nDas Bild funktioniert trotzdem; für den Ton dieses Setup später erneut ausführen.
en.VbFailed=VB-CABLE could not be installed (code %1).%nDetails: %2
de.VbFailed=VB-CABLE konnte nicht installiert werden (Code %1).%nDetails: %2
en.StatusSecure=Securing folders …
de.StatusSecure=Ordner werden abgesichert …
en.StatusMonitor=Setting up the virtual monitor (Windows may ask for confirmation) …
de.StatusMonitor=Virtueller Monitor wird eingerichtet (Windows fragt evtl. nach) …
en.MonitorFailed=The monitor driver could not be installed (code %1).%nDetails: %2
de.MonitorFailed=Der Monitor-Treiber konnte nicht installiert werden (Code %1).%nDetails: %2
en.StatusSpeaker=Installing virtual speakers (VB-CABLE) …
de.StatusSpeaker=Virtueller Lautsprecher (VB-CABLE) wird installiert …
en.StatusService=Setting up the USBCD helper service …
de.StatusService=USBCD-Hilfsdienst wird eingerichtet …
en.ServiceFailed=The USBCD helper service could not be set up – without it the tablet monitor cannot be switched on.%nDetails: %1
de.ServiceFailed=Der USBCD-Hilfsdienst konnte nicht eingerichtet werden – ohne ihn lässt sich der Tablet-Monitor nicht einschalten.%nDetails: %1
en.SecureFailed=The folders could not be secured.%nDetails: %1
de.SecureFailed=Die Ordner konnten nicht abgesichert werden.%nDetails: %1
en.AppDirInsecure=The installation folder could not be secured (only administrators may change it, because the USBCD helper service and the webcam run with system rights).%n%nPlease choose a different folder, e.g. under "Program Files".
de.AppDirInsecure=Der Installationsordner konnte nicht abgesichert werden (nur Administratoren dürfen ihn ändern, weil der USBCD-Hilfsdienst und die Webcam mit Systemrechten laufen).%n%nBitte einen anderen Ordner wählen, z. B. unter „Programme“.
en.AskRemoveVb=Also remove the virtual speakers (VB-CABLE)?%n%nUSBCD's setup installed them for the tablet sound. Choose “No” only if other programs use VB-CABLE – it is then reset to its original state (name “CABLE Input”).
de.AskRemoveVb=Den virtuellen Lautsprecher (VB-CABLE) ebenfalls entfernen?%n%nDas USBCD-Setup hat ihn für den Tablet-Ton installiert. Nur „Nein“ wählen, wenn andere Programme VB-CABLE nutzen – dann wird er auf den Auslieferungszustand zurückgesetzt (Name „CABLE Input“).
en.AskRemoveApp=Also remove the USBCD app from the connected tablet?%n%n(Only works if the tablet is connected now.)
de.AskRemoveApp=Die USBCD-App auch vom angeschlossenen Tablet entfernen?%n%n(Klappt nur, wenn das Tablet gerade angeschlossen ist.)

[Tasks]
Name: "vbcable"; Description: "{cm:TaskVbCable}"; Check: not VbCableInstalled
Name: "autostart"; Description: "{cm:TaskAutostart}"

[Files]
Source: "..\host\target\release\{#HostExe}"; DestDir: "{app}"; Flags: ignoreversion{#SignFlag}
; Webcam-Quelle; ist sie gerade vom Kameradienst geladen, wird sie beim Neustart ersetzt
Source: "..\host\target\release\usbcd_cam.dll"; DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete{#SignFlag}
Source: "deps\ffmpeg.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "deps\platform-tools\*"; DestDir: "{app}\platform-tools"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "deps\vdd\*"; DestDir: "{app}\drivers\vdd"; Flags: ignoreversion
; Der Monitor-Treiber liest seine Einstellungen fest aus C:\VirtualDisplayDriver
Source: "deps\vdd\vdd_settings.xml"; DestDir: "C:\VirtualDisplayDriver"; Flags: onlyifdoesntexist
Source: "setup-helper.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "diagnose.ps1"; DestDir: "{app}"; Flags: ignoreversion
; Tablet-App – wird beim Anstecken automatisch installiert bzw. aktualisiert
Source: "deps\USBCD.apk"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "deps\USBCD.apk.version"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "USBCD.ico"; DestDir: "{app}"; Flags: ignoreversion
; Symbole für den Infobereich: dunkle bzw. helle Taskleiste
Source: "USBCD-tray.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "USBCD-tray-light.ico"; DestDir: "{app}"; Flags: ignoreversion
; Logo für die Windows-Benachrichtigungen
Source: "USBCD-logo.png"; DestDir: "{app}"; Flags: ignoreversion
Source: "THIRD-PARTY.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\host\assets\USBCDGrotesk-OFL.txt"; DestDir: "{app}"; DestName: "OFL.txt"; Flags: ignoreversion
Source: "..\README.de.md"; DestDir: "{app}"; DestName: "LIESMICH.md"; Flags: ignoreversion; Languages: de
Source: "..\README.md"; DestDir: "{app}"; DestName: "README.md"; Flags: ignoreversion; Languages: en

[Icons]
; startet USBCD bzw. lässt die laufende Instanz ihre Info-Blase zeigen
Name: "{autoprograms}\USBCD"; Filename: "{app}\{#HostExe}"; Parameters: "--show"; IconFilename: "{app}\USBCD.ico"; Comment: "{cm:IconComment}"; AppUserModelID: "Samuel.USBCD"
Name: "{autoprograms}\{cm:IconSettings}"; Filename: "{app}\{#HostExe}"; Parameters: "--open-folder"; IconFilename: "{app}\USBCD.ico"
Name: "{autoprograms}\{cm:IconDiagnose}"; Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\diagnose.ps1"""; IconFilename: "{app}\USBCD.ico"

[Registry]
; Autostart-Eintrag (legt USBCD.exe --autostart an) beim Deinstallieren entfernen
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "USBCD"; Flags: uninsdeletevalue dontcreatekey
; Merker des Task-Managers („Autostart deaktiviert“)
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"; ValueType: none; ValueName: "USBCD"; Flags: uninsdeletevalue dontcreatekey
; Absender „USBCD“ der Benachrichtigungen und Link-Protokoll usbcd: (legt USBCD.exe an)
Root: HKCU; Subkey: "Software\Classes\AppUserModelId\Samuel.USBCD"; ValueType: none; Flags: uninsdeletekey dontcreatekey
Root: HKCU; Subkey: "Software\Classes\usbcd"; ValueType: none; Flags: uninsdeletekey dontcreatekey
; Webcam-Quelle für den Windows-Kameradienst (der liest nur HKLM). Die Kamera selbst legt
; USBCD erst an, wenn sie im Menü eingeschaltet wird – sonst bleibt das hier ungenutzt.
Root: HKLM64; Subkey: "Software\Classes\CLSID\{{CE63EC35-A188-4E54-8E6F-8F3C01EF1CBA}"; ValueType: string; ValueName: ""; ValueData: "USBCD Tablet Webcam"; Flags: uninsdeletekey
Root: HKLM64; Subkey: "Software\Classes\CLSID\{{CE63EC35-A188-4E54-8E6F-8F3C01EF1CBA}\InprocServer32"; ValueType: string; ValueName: ""; ValueData: "{app}\usbcd_cam.dll"; Flags: uninsdeletekey
Root: HKLM64; Subkey: "Software\Classes\CLSID\{{CE63EC35-A188-4E54-8E6F-8F3C01EF1CBA}\InprocServer32"; ValueType: string; ValueName: "ThreadingModel"; ValueData: "Both"

[UninstallRun]
Filename: "{app}\{#HostExe}"; Parameters: "--uninstall-service"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveService"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#HostExe}"; Flags: runhidden; RunOnceId: "KillHost"
; Kameradienst gibt usbcd_cam.dll frei (startet bei Bedarf von selbst wieder)
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -Command ""Stop-Service -Name FrameServer -Force -ErrorAction SilentlyContinue"""; Flags: runhidden waituntilterminated; RunOnceId: "UnloadCamera"
Filename: "{app}\platform-tools\adb.exe"; Parameters: "kill-server"; Flags: runhidden; RunOnceId: "KillAdb"
Filename: "{sys}\schtasks.exe"; Parameters: "/Delete /TN {#OldTask} /F"; Flags: runhidden; RunOnceId: "DeleteOldTask"
Filename: "{app}\{#HostExe}"; Parameters: "--remove-device Root\MttVDD"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveVdd"
; Treiberpaket des Monitors aus dem Windows-Treiberspeicher (nur mttvdd.inf – Parsec bleibt)
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\setup-helper.ps1"" -Action vdd-remove"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveVddPackage"

[UninstallDelete]
Type: filesandordirs; Name: "{app}\vbcable"
Type: filesandordirs; Name: "{localappdata}\USBCD"
Type: filesandordirs; Name: "{commonappdata}\USBCD"
Type: filesandordirs; Name: "C:\VirtualDisplayDriver"
; Reste älterer Versionen bzw. des Setup-Bauens
Type: filesandordirs; Name: "{%TEMP}\USBCD-vbcable"
Type: filesandordirs; Name: "{%TEMP}\USBCD-build"
Type: files; Name: "{%TEMP}\usbcd-task.xml"
Type: dirifempty; Name: "{app}"

[Code]
var
  DownloadPage: TDownloadWizardPage;
  RebootNeeded: Boolean;

function VbCableInstalled: Boolean;
var
  Rec: TFindRec;
begin
  // neuere VB-CABLE-Versionen liegen nur im Treiberspeicher, ältere unter drivers
  Result := FindFirst(ExpandConstant('{sys}\DriverStore\FileRepository\vbmmecable*'), Rec);
  if Result then
    FindClose(Rec)
  else
  begin
    Result := FindFirst(ExpandConstant('{sys}\drivers\vbaudio_cable*.sys'), Rec);
    if Result then
      FindClose(Rec);
  end;
end;

procedure InitializeWizard;
begin
  DownloadPage := CreateDownloadPage(SetupMessage(msgWizardPreparing), SetupMessage(msgPreparingDesc), nil);
end;

function TryDownload(const Url: String): Boolean;
begin
  DownloadPage.Clear;
  DownloadPage.Add(Url, 'vbcable.zip', '');
  try
    DownloadPage.Download;
    Result := True;
  except
    Log('Download fehlgeschlagen: ' + Url + ' – ' + GetExceptionMessage);
    Result := False;
  end;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if (CurPageID = wpReady) and WizardIsTaskSelected('vbcable') then
  begin
    DownloadPage.Show;
    try
      // geprüft wird später die Signatur der entpackten Setup-Datei (VB-Audio)
      if not TryDownload('https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip') then
        if not TryDownload('https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip') then
          SuppressibleMsgBox(CustomMessage('VbDownloadFailed'), mbError, MB_OK, IDOK);
    finally
      DownloadPage.Hide;
    end;
  end;
end;

function RunQuiet(const Exe, Params: String): Integer;
var
  RC: Integer;
begin
  if not Exec(Exe, Params, '', SW_HIDE, ewWaitUntilTerminated, RC) then
    RC := -1;
  Log(Exe + ' ' + Params + ' → ' + IntToStr(RC));
  Result := RC;
end;

function PowerShell: String;
begin
  Result := ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe');
end;

function RunHelper(const Args: String): Integer;
begin
  Result := RunQuiet(PowerShell, '-NoProfile -ExecutionPolicy Bypass -File "' +
    ExpandConstant('{app}\setup-helper.ps1') + '" ' + Args);
end;

function IsReparsePoint(const Path: String): Boolean;
var
  Rec: TFindRec;
begin
  Result := False;
  if FindFirst(RemoveBackslashUnlessRoot(Path), Rec) then
  begin
    Result := (Rec.Attributes and FILE_ATTRIBUTE_REPARSE_POINT) <> 0;
    FindClose(Rec);
  end;
end;

// Programmordner: nur SYSTEM und Administratoren dürfen schreiben – auch wenn er nicht
// unter „Programme“ liegt (dort erben neue Ordner oft Schreibrechte für alle Benutzer).
// Hilfsdienst (LocalSystem) und Webcam-DLL (Kameradienst) laufen von hier.
function SecureAppDir: Boolean;
var
  Dir, Icacls: String;
begin
  Result := False;
  Dir := ExpandConstant('{app}');
  Icacls := ExpandConstant('{sys}\icacls.exe');
  if IsReparsePoint(Dir) then
    exit;
  if not ForceDirectories(Dir) then
    exit;
  RunQuiet(Icacls, '"' + Dir + '" /setowner *S-1-5-32-544 /T /C /Q');
  if RunQuiet(Icacls, '"' + Dir + '" /inheritance:r /grant:r *S-1-5-18:(OI)(CI)F *S-1-5-32-544:(OI)(CI)F *S-1-5-32-545:(OI)(CI)RX *S-1-5-19:(OI)(CI)RX /Q') <> 0 then
    exit;
  // vorhandene Dateien (Update) erben nur noch diese Rechte
  RunQuiet(Icacls, '"' + Dir + '\*" /reset /T /C /Q');
  Result := True;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  if not SecureAppDir then
  begin
    Result := CustomMessage('AppDirInsecure');
    exit;
  end;
  // laufende Teile beenden, damit die Dateien ersetzt werden können
  RunQuiet(PowerShell, '-NoProfile -Command "Stop-Service -Name USBCDHelper -Force -ErrorAction SilentlyContinue"');
  // ältere Versionen liefen über eine Aufgabe mit Administratorrechten – entfernen
  RunQuiet(ExpandConstant('{sys}\schtasks.exe'), '/End /TN {#OldTask}');
  RunQuiet(ExpandConstant('{sys}\schtasks.exe'), '/Delete /TN {#OldTask} /F');
  RunQuiet(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#HostExe}');
  // Update: der Windows-Kameradienst hält usbcd_cam.dll evtl. geladen – kurz beenden,
  // er startet beim nächsten Kamerazugriff von selbst wieder
  if FileExists(ExpandConstant('{app}\usbcd_cam.dll')) then
    RunQuiet(PowerShell, '-NoProfile -Command "Stop-Service -Name FrameServer -Force -ErrorAction SilentlyContinue"');
  if FileExists(ExpandConstant('{app}\platform-tools\adb.exe')) then
    RunQuiet(ExpandConstant('{app}\platform-tools\adb.exe'), 'kill-server');
  // Eine vorab angelegte Verknüpfung an Stelle des Treiber-Ordners nie beschreiben
  if IsReparsePoint('C:\VirtualDisplayDriver') then
    RunQuiet(ExpandConstant('{cmd}'), '/c rmdir "C:\VirtualDisplayDriver"');
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Host, Details: String;
  R: Integer;
begin
  if CurStep <> ssPostInstall then
    exit;
  Host := ExpandConstant('{app}\{#HostExe}');
  Details := ExpandConstant('{commonappdata}\USBCD\setup.log');

  WizardForm.StatusLabel.Caption := CustomMessage('StatusSecure');
  R := RunHelper('-Action harden -Dest "' + ExpandConstant('{app}') + '"');
  if R <> 0 then
    SuppressibleMsgBox(FmtMessage(CustomMessage('SecureFailed'), [ExpandConstant('{commonappdata}\USBCD\setup-helper.log')]), mbError, MB_OK, IDOK);

  WizardForm.StatusLabel.Caption := CustomMessage('StatusMonitor');
  R := RunQuiet(Host, '--install-driver "' + ExpandConstant('{app}\drivers\vdd\MttVDD.inf') + '" Root\MttVDD');
  if R = 3010 then
    RebootNeeded := True
  else if R <> 0 then
    SuppressibleMsgBox(FmtMessage(CustomMessage('MonitorFailed'), [IntToStr(R), Details]), mbError, MB_OK, IDOK);

  if WizardIsTaskSelected('vbcable') and FileExists(ExpandConstant('{tmp}\vbcable.zip')) then
  begin
    WizardForm.StatusLabel.Caption := CustomMessage('StatusSpeaker');
    R := RunHelper('-Action vbcable -Zip "' + ExpandConstant('{tmp}\vbcable.zip') + '" -Dest "' + ExpandConstant('{app}\vbcable') + '"');
    if R <> 0 then
      SuppressibleMsgBox(FmtMessage(CustomMessage('VbFailed'), [IntToStr(R), ExpandConstant('{commonappdata}\USBCD\setup-helper.log')]), mbError, MB_OK, IDOK);
    RebootNeeded := True; // VB-Audio empfiehlt einen Neustart
  end;

  WizardForm.StatusLabel.Caption := CustomMessage('StatusService');
  if RunQuiet(Host, '--install-service') <> 0 then
    SuppressibleMsgBox(FmtMessage(CustomMessage('ServiceFailed'), [Details]), mbError, MB_OK, IDOK);

  // Autostart und Start als angemeldeter Nutzer – ohne Administratorrechte
  if WizardIsTaskSelected('autostart') then
    ExecAsOriginalUser(Host, '--autostart on', '', SW_HIDE, ewWaitUntilTerminated, R)
  else
    ExecAsOriginalUser(Host, '--autostart off', '', SW_HIDE, ewWaitUntilTerminated, R);
  ExecAsOriginalUser(Host, '', '', SW_SHOWNORMAL, ewNoWait, R);
end;

function NeedRestart(): Boolean;
begin
  Result := RebootNeeded;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Adb: String;
begin
  if CurUninstallStep <> usUninstall then
    exit;
  Adb := ExpandConstant('{app}\platform-tools\adb.exe');
  if FileExists(Adb) and not UninstallSilent then
    if MsgBox(CustomMessage('AskRemoveApp'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      RunQuiet(Adb, 'forward --remove-all');
      RunQuiet(Adb, 'uninstall de.samuel.usbcd');
    end;
  // VB-CABLE: entfernen oder (wenn es bleibt) auf den Auslieferungszustand zurücksetzen
  if VbCableInstalled then
  begin
    if (not UninstallSilent) and (MsgBox(CustomMessage('AskRemoveVb'), mbConfirmation, MB_YESNO) = IDYES) then
      RunHelper('-Action vbcable-remove -Dest "' + ExpandConstant('{app}\vbcable') + '"')
    else
      RunQuiet(ExpandConstant('{app}\{#HostExe}'), '--restore-audio');
  end;
end;
