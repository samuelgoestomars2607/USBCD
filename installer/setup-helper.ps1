# Hilfsskript des USBCD-Installers (läuft mit Administratorrechten aus dem
# Programmordner, den nur Administratoren ändern können).
#
#   -Action harden          Ordner absichern (ProgramData\USBCD, C:\VirtualDisplayDriver);
#                           mit -Dest: Webcam-DLL für den Kameradienst (LocalService) lesbar
#   -Action vbcable         VB-CABLE aus dem heruntergeladenen Archiv installieren
#   -Action vbcable-remove  VB-CABLE wieder entfernen (Gerät und Treiberpaket)
#   -Action vdd-remove      Treiberpaket des Monitor-Treibers aus dem Treiberspeicher löschen
param(
    [Parameter(Mandatory = $true)][ValidateSet('harden', 'vbcable', 'vbcable-remove', 'vdd-remove')][string]$Action,
    [string]$Zip,
    [string]$Dest
)
$ErrorActionPreference = 'Stop'
$logDir = Join-Path $env:ProgramData 'USBCD'
if ($Action -ne 'harden' -and (Test-Path -LiteralPath $logDir)) {
    Start-Transcript -Path (Join-Path $logDir 'setup-helper.log') -Append | Out-Null
}
trap { Write-Output "FEHLER: $_"; try { Stop-Transcript | Out-Null } catch {}; exit 1 }

function Test-Reparse([string]$Path) {
    $i = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    return ($i -and ($i.Attributes -band [IO.FileAttributes]::ReparsePoint))
}

# Verknüpfungen (Junctions/Symlinks) im Ordner entfernen, ohne ihnen zu folgen
function Remove-Reparse([string]$Path) {
    Get-ChildItem -LiteralPath $Path -Force -Recurse -Attributes ReparsePoint -ErrorAction SilentlyContinue |
        ForEach-Object {
            Write-Output "   Verknüpfung entfernt: $($_.FullName)"
            if ($_.PSIsContainer) { cmd /c rmdir "$($_.FullName)" } else { cmd /c del /f /q "$($_.FullName)" }
        }
}

# Nur SYSTEM und Administratoren dürfen schreiben; Benutzer und der Treiberdienst lesen.
function Protect-Dir([string]$Path) {
    if (Test-Reparse $Path) { cmd /c rmdir "$Path"; Write-Output "   Verknüpfung entfernt: $Path" }
    if (-not (Test-Path -LiteralPath $Path)) { New-Item -ItemType Directory -Path $Path -Force | Out-Null }
    Remove-Reparse $Path
    icacls "$Path" /setowner '*S-1-5-32-544' /T /C /Q | Out-Null
    icacls "$Path" /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' `
        '*S-1-5-32-545:(OI)(CI)RX' '*S-1-5-19:(OI)(CI)RX' /Q | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls $Path fehlgeschlagen" }
    # Inhalt erbt nur noch diese Rechte
    if (Get-ChildItem -LiteralPath $Path -Force) { icacls "$Path\*" /reset /T /C /Q | Out-Null }
    Write-Output "   abgesichert: $Path"
}

# Signatur von VB-Audio (Vincent Burel) prüfen
# Treiberpakete (oemNN.inf) anhand des ursprünglichen INF-Namens aus dem Treiberspeicher
# löschen – so werden nie fremde Treiber (z. B. Parsec) erfasst.
function Remove-DriverPackage([string]$InfPattern) {
    $found = $false
    Get-WindowsDriver -Online -ErrorAction SilentlyContinue |
        Where-Object { (Split-Path $_.OriginalFileName -Leaf) -match $InfPattern -and $_.Driver -match '^oem\d+\.inf$' } |
        ForEach-Object {
            $found = $true
            Write-Output "   Treiberpaket $($_.Driver) ($(Split-Path $_.OriginalFileName -Leaf))"
            pnputil.exe /delete-driver $_.Driver /uninstall /force | Out-Host
        }
    if (-not $found) { Write-Output "   kein Treiberpaket $InfPattern gefunden" }
}

function Assert-VbAudio([string]$File) {
    $sig = Get-AuthenticodeSignature -LiteralPath $File
    $subj = if ($sig.SignerCertificate) { $sig.SignerCertificate.Subject } else { '' }
    Write-Output "   Signatur $($sig.Status): $subj"
    if ($sig.Status -ne 'Valid' -or $subj -notmatch 'BUREL') {
        throw "$File ist nicht gültig von VB-Audio signiert – abgebrochen"
    }
}

switch ($Action) {
    'harden' {
        # Log-Ordner des Hilfsdienstes: immer frisch anlegen (rd folgt keinen Verknüpfungen)
        if (Test-Path -LiteralPath $logDir) { cmd /c rd /s /q "$logDir" }
        Protect-Dir $logDir
        Start-Transcript -Path (Join-Path $logDir 'setup-helper.log') -Append | Out-Null
        # Einstellungen des Monitor-Treibers: lag bisher mit den Rechten von C:\ (jeder darf ändern)
        Protect-Dir 'C:\VirtualDisplayDriver'
        # Webcam: Protokoll der Kameraquelle – nur diese eine Datei darf der Kameradienst
        # (LocalService) beschreiben; die DLL legt selbst keine Dateien an
        $camLog = Join-Path $logDir 'kamera.log'
        New-Item -ItemType File -Path $camLog -Force | Out-Null
        icacls "$camLog" /grant '*S-1-5-19:M' /Q | Out-Null
        if ($Dest) {
            $dll = Join-Path $Dest 'usbcd_cam.dll'
            if (Test-Path -LiteralPath $dll) {
                icacls "$dll" /grant '*S-1-5-19:RX' /Q | Out-Null
                Write-Output "   Webcam-DLL für den Kameradienst lesbar: $dll"
            }
        }
    }
    'vbcable' {
        # In einen Ordner entpacken, den nur Administratoren ändern dürfen – so kann
        # zwischen Prüfung und Start niemand die Datei austauschen.
        if (-not $Dest) { throw '-Dest fehlt' }
        if (Test-Path -LiteralPath $Dest) { cmd /c rd /s /q "$Dest" }
        New-Item -ItemType Directory -Path $Dest -Force | Out-Null
        Expand-Archive -Path $Zip -DestinationPath $Dest -Force
        Remove-Reparse $Dest
        $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'VBCABLE_Setup_ARM64.exe' } else { 'VBCABLE_Setup_x64.exe' }
        $setup = Get-ChildItem -LiteralPath $Dest -Recurse -Filter $arch | Select-Object -First 1
        if (-not $setup) { throw "$arch nicht im Archiv" }
        Assert-VbAudio $setup.FullName
        $p = Start-Process -FilePath $setup.FullName -ArgumentList '-i', '-h' -WorkingDirectory $setup.DirectoryName -Wait -PassThru
        Write-Output "VB-CABLE-Setup beendet mit $($p.ExitCode)"
        # Merker: dieses VB-CABLE hat USBCD installiert (für die Deinstallation)
        Set-Content -LiteralPath (Join-Path $Dest 'installiert-von-usbcd.txt') -Value $setup.FullName
        Stop-Transcript | Out-Null
        exit $p.ExitCode
    }
    'vbcable-remove' {
        # Mit eigenem VB-CABLE-Setup (falls USBCD es installiert hat) offiziell deinstallieren …
        $marker = if ($Dest) { Join-Path $Dest 'installiert-von-usbcd.txt' } else { '' }
        if ($marker -and (Test-Path -LiteralPath $marker)) {
            $exe = (Get-Content -LiteralPath $marker -TotalCount 1).Trim()
            if ((Test-Path -LiteralPath $exe) -and $exe.StartsWith($Dest, [StringComparison]::OrdinalIgnoreCase)) {
                Assert-VbAudio $exe
                $p = Start-Process -FilePath $exe -ArgumentList '-u', '-h' -WorkingDirectory (Split-Path $exe) -Wait -PassThru
                Write-Output "VB-CABLE-Deinstallation beendet mit $($p.ExitCode)"
            }
        }
        # … und alles Übrige entfernen – nur Geräte mit der VB-CABLE-Hardware-ID, nie andere Audiogeräte
        Get-PnpDevice -ErrorAction SilentlyContinue |
            Where-Object { ($_.HardwareID -join ';') -match '(^|;)VBAudioVACWDM(;|$)' } |
            ForEach-Object {
                Write-Output "   Gerät $($_.FriendlyName)"
                pnputil.exe /remove-device "$($_.InstanceId)" | Out-Host
            }
        Remove-DriverPackage '^vbmmecable(64|arm64)?_win'
    }
    'vdd-remove' {
        # Gerät entfernt bereits USBCD.exe --remove-device; hier das Paket im Treiberspeicher
        Remove-DriverPackage '^mttvdd\.inf$'
    }
}
try { Stop-Transcript | Out-Null } catch {}
exit 0
