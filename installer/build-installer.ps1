# Baut USBCD-Setup-<version>.exe
#   Rechtsklick → "Mit PowerShell ausführen"  (oder: powershell -ExecutionPolicy Bypass -File build-installer.ps1)
#
# Schritte: USBCD bauen → FFmpeg, Android Platform-Tools und Virtual Display Driver
# nach deps\ laden (nur beim ersten Mal) und prüfen → Tablet-App bauen und signieren →
# Inno Setup kompilieren (wird bei Bedarf per winget installiert).
# Ergebnis: installer\Output\USBCD-Setup-<version>.exe
#
# Feste Versionen: FFmpeg und adb kommen aus dem Release „build-deps-1“ dieses Projekts
# (adb zuerst direkt von Google), der Monitor-Treiber aus einem festen Release seines
# Projekts. So baut jeder mit exakt denselben Dateien.
# Prüfung der Abhängigkeiten: Signaturen (adb von Google, Monitor-Treiber) und SHA-256-
# Prüfsummen in deps.lock.json. Weicht eine Datei ab, bricht der Build ab
# (bewusstes Update auf neue Versionen: URLs unten ändern und -UpdateDeps).
#
# Optional: Code-Signatur von USBCD.exe und Setup mit einem eigenen Zertifikat:
#   $env:USBCD_SIGN_CERT_THUMBPRINT = '<SHA1-Fingerabdruck im Zertifikatspeicher>'

param(
    [switch]$Refresh,     # Abhängigkeiten neu herunterladen (übernimmt neue Prüfsummen)
    [switch]$UpdateDeps   # geänderte Prüfsummen bewusst übernehmen
)
if ($Refresh) { $UpdateDeps = $true }
$ErrorActionPreference = 'Stop'
Start-Transcript -Path (Join-Path (Split-Path -Parent $PSCommandPath) 'build.log') -Force | Out-Null
# Bei Fehlern Fenster offen lassen, damit die Meldung lesbar bleibt
trap {
    Write-Host "`nFEHLER: $_" -ForegroundColor Red
    Write-Host "Protokoll: installer\build.log"
    Stop-Transcript | Out-Null
    Read-Host 'Enter zum Schließen'
    exit 1
}
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$here = Split-Path -Parent $PSCommandPath
$repo = Split-Path -Parent $here
$deps = Join-Path $here 'deps'
$tmp = Join-Path $env:TEMP 'USBCD-build'
# Release mit den festen Fremdprogrammen (FFmpeg samt Quellcode, adb)
$depsRelease = 'https://github.com/samuelgoestomars2607/USBCD/releases/download/build-deps-1'
if ($Refresh) { Remove-Item $deps -Recurse -Force -ErrorAction SilentlyContinue }
New-Item -ItemType Directory -Force -Path $deps, $tmp | Out-Null

function Step($t) { Write-Host "`n==> $t" -ForegroundColor Cyan }

# --- Prüfsummen und Signaturen ---
$lockFile = Join-Path $here 'deps.lock.json'
$lock = @{}
if (Test-Path $lockFile) {
    (Get-Content $lockFile -Raw | ConvertFrom-Json).PSObject.Properties | ForEach-Object { $lock[$_.Name] = $_.Value }
}

function Get-TreeHash([string]$Path) {
    $item = Get-Item -LiteralPath $Path
    if (-not $item.PSIsContainer) { return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash }
    $root = $item.FullName.TrimEnd('\')
    $lines = Get-ChildItem -LiteralPath $root -Recurse -File | Sort-Object FullName | ForEach-Object {
        $_.FullName.Substring($root.Length) + ':' + (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
    }
    $sha = [Security.Cryptography.SHA256]::Create()
    $bytes = [Text.Encoding]::UTF8.GetBytes(($lines -join "`n"))
    return ([BitConverter]::ToString($sha.ComputeHash($bytes)) -replace '-', '')
}

function Confirm-Pinned([string]$Name, [string]$Path) {
    $h = Get-TreeHash $Path
    if ($lock.ContainsKey($Name)) {
        if ($lock[$Name] -eq $h) { Write-Host "   ${Name}: Prüfsumme stimmt"; return }
        if (-not $UpdateDeps) {
            throw "${Name} hat sich verändert (SHA-256 $h, gemerkt: $($lock[$Name])). Falls du die Datei bewusst neu geladen hast: build-installer.ps1 -UpdateDeps"
        }
        Write-Warning "   ${Name}: neue Prüfsumme übernommen (-UpdateDeps)"
    } else {
        Write-Host "   ${Name}: Prüfsumme erstmals gemerkt"
    }
    $lock[$Name] = $h
    $sorted = [ordered]@{}
    $lock.Keys | Sort-Object | ForEach-Object { $sorted[$_] = $lock[$_] }
    $sorted | ConvertTo-Json | Set-Content $lockFile -Encoding UTF8
}

function Assert-Signed([string]$File, [string]$SubjectPattern) {
    $sig = Get-AuthenticodeSignature -LiteralPath $File
    $subj = if ($sig.SignerCertificate) { $sig.SignerCertificate.Subject } else { '-' }
    if ($sig.Status -ne 'Valid' -or ($SubjectPattern -and $subj -notmatch $SubjectPattern)) {
        throw "${File}: Signatur ungültig ($($sig.Status), $subj)"
    }
    Write-Host "   $(Split-Path $File -Leaf) signiert von: $subj"
}

function Get-FirstUrl([string[]]$Urls, [string]$OutFile) {
    foreach ($u in $Urls) {
        try {
            Write-Host "   lade $u"
            Invoke-WebRequest -Uri $u -OutFile $OutFile -UseBasicParsing
            return
        } catch {
            Write-Warning "   nicht verfügbar: $($_.Exception.Message)"
        }
    }
    throw "Download fehlgeschlagen: $($Urls -join ', ')"
}

# --- 1. USBCD (Rust) ---
Step 'USBCD bauen (cargo build --release --locked)'
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'cargo nicht gefunden – Rust von rustup.rs installieren' }
Push-Location (Join-Path $repo 'host')
# --locked: genau die geprüften Versionen aus Cargo.lock (keine stillen Updates)
cargo build --release --locked
$ok = $LASTEXITCODE -eq 0
Pop-Location
if (-not $ok) { throw 'cargo build fehlgeschlagen' }
if (-not (Test-Path (Join-Path $repo 'host\target\release\usbcd_cam.dll'))) { throw 'usbcd_cam.dll (Webcam) wurde nicht gebaut' }
$version = (Select-String -Path (Join-Path $repo 'host\Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value

# --- 2. FFmpeg (BtbN-Build N-129130-gcd39c36377, GPL, statisch) ---
# Benötigt gfxcapture mit "dup_frames" (gleichmäßiger Bildstrom = geringe Verzögerung).
# Feste Version aus dem Release build-deps-1; dort liegt auch ihr Quellcode (GPL).
if (Test-Path "$deps\ffmpeg.exe") {
    $ErrorActionPreference = 'Continue'
    $chk = & "$deps\ffmpeg.exe" -hide_banner -h filter=gfxcapture 2>&1 | Out-String
    $ErrorActionPreference = 'Stop'
    if ($chk -notmatch 'dup_frames') {
        Write-Host '   vorhandenes FFmpeg zu alt (ohne dup_frames) – lade neu'
        Remove-Item "$deps\ffmpeg.exe" -Force
    }
}
if (-not (Test-Path "$deps\ffmpeg.exe")) {
    Step 'FFmpeg herunterladen'
    $zip = "$tmp\ffmpeg.zip"
    Get-FirstUrl @("$depsRelease/ffmpeg-N-129130-gcd39c36377-win64-gpl.zip") $zip
    Remove-Item "$tmp\ffmpeg" -Recurse -Force -ErrorAction SilentlyContinue
    Expand-Archive $zip "$tmp\ffmpeg" -Force
    $exe = Get-ChildItem "$tmp\ffmpeg" -Recurse -Filter ffmpeg.exe | Select-Object -First 1
    Copy-Item $exe.FullName "$deps\ffmpeg.exe"
}
Confirm-Pinned 'ffmpeg.exe' "$deps\ffmpeg.exe"   # unsigniert – nur Prüfsumme
$ErrorActionPreference = 'Continue'   # ffmpeg schreibt auf stderr – kein Abbruch deswegen
$ff = & "$deps\ffmpeg.exe" -hide_banner -h filter=gfxcapture 2>&1 | Out-String
$ErrorActionPreference = 'Stop'
if ($ff -notmatch 'gfxcapture' -or $ff -match 'Unknown filter') { throw 'FFmpeg ohne gfxcapture – mit -Refresh neu laden' }

# --- 3. Android Platform-Tools (adb) ---
if (-not (Test-Path "$deps\platform-tools\adb.exe")) {
    Step 'Android Platform-Tools herunterladen'
    $zip = "$tmp\platform-tools.zip"
    # feste Version 37.0.3: zuerst von Google, sonst aus dem Release build-deps-1
    Get-FirstUrl @(
        'https://dl.google.com/android/repository/platform-tools_r37.0.3-win.zip',
        "$depsRelease/platform-tools_r37.0.3-win.zip"
    ) $zip
    Expand-Archive $zip $deps -Force   # enthält den Ordner platform-tools\
}
Assert-Signed "$deps\platform-tools\adb.exe" 'Google'
Confirm-Pinned 'platform-tools' "$deps\platform-tools"

# --- 4. Virtual Display Driver (MIT, signiert) ---
if (-not (Test-Path "$deps\vdd\MttVDD.inf")) {
    Step 'Virtual Display Driver herunterladen'
    $zip = "$tmp\vdd.zip"
    Get-FirstUrl @('https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/25.7.23/VirtualDisplayDriver-x86.Driver.Only.zip') $zip
    Remove-Item "$tmp\vdd" -Recurse -Force -ErrorAction SilentlyContinue
    Expand-Archive $zip "$tmp\vdd" -Force
    New-Item -ItemType Directory -Force -Path "$deps\vdd" | Out-Null
    $inf = Get-ChildItem "$tmp\vdd" -Recurse -Filter MttVDD.inf | Select-Object -First 1
    Copy-Item "$($inf.DirectoryName)\*" "$deps\vdd" -Recurse -Force
}
$cat = Get-ChildItem "$deps\vdd" -Filter *.cat | Select-Object -First 1
if (-not $cat) { throw 'Monitor-Treiber ohne Signaturkatalog (.cat) – mit -Refresh neu laden' }
Assert-Signed $cat.FullName $null
Confirm-Pinned 'vdd' "$deps\vdd"

# --- 5. Tablet-App (APK) bauen ---
# Nutzt das Android SDK und Java aus Android Studio. Schlägt das fehl, entsteht das Setup
# trotzdem – nur ohne mitgelieferte App (dann wie bisher über Android Studio installieren).
Step 'Tablet-App bauen (Gradle)'
Remove-Item "$deps\USBCD.apk", "$deps\USBCD.apk.version" -Force -ErrorAction SilentlyContinue
$android = Join-Path $repo 'android'
if (-not $env:JAVA_HOME) {
    foreach ($j in @("$env:ProgramFiles\Android\Android Studio\jbr", "$env:LOCALAPPDATA\Programs\Android Studio\jbr")) {
        if (Test-Path "$j\bin\java.exe") { $env:JAVA_HOME = $j; break }
    }
}
if (-not (Test-Path "$android\local.properties")) {
    $sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { Join-Path $env:LOCALAPPDATA 'Android\Sdk' }
    if (Test-Path $sdk) { "sdk.dir=$($sdk -replace '\\','\\\\')" | Set-Content "$android\local.properties" -Encoding ASCII }
}
# Eigener Release-Schlüssel (statt des öffentlich bekannten Debug-Schlüssels).
# Liegt in installer\keys\ – sichern! Ohne ihn lassen sich spätere App-Versionen nicht
# als Update einspielen (USBCD installiert die App dann automatisch neu).
$keys = Join-Path $here 'keys'
$ks = Join-Path $keys 'usbcd-release.p12'
$pwFile = Join-Path $keys 'keystore-passwort.txt'
$keytool = if ($env:JAVA_HOME) { Join-Path $env:JAVA_HOME 'bin\keytool.exe' } else { 'keytool.exe' }
if (-not (Test-Path $ks)) {
    Step 'Release-Schlüssel der Tablet-App anlegen'
    New-Item -ItemType Directory -Force -Path $keys | Out-Null
    # nur der eigene Benutzer darf den Schlüssel lesen
    icacls $keys /inheritance:r /grant:r "$($env:USERDOMAIN)\$($env:USERNAME):(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' | Out-Null
    $b = New-Object byte[] 24
    [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($b)
    $pw = [Convert]::ToBase64String($b) -replace '[+/=]', 'x'
    Set-Content -LiteralPath $pwFile -Value $pw -Encoding ASCII -NoNewline
    $ErrorActionPreference = 'Continue'
    # keytool meldet auch Erfolg auf stderr; als Text ausgeben, damit PowerShell es nicht rot als Fehler zeigt.
    # Der Schlüssel trägt keinen Personennamen – jeder, der selbst baut, bekommt seinen eigenen.
    & $keytool -J-Duser.language=en -genkeypair -keystore $ks -storetype PKCS12 -alias usbcd -keyalg RSA -keysize 3072 `
        -validity 10000 -dname 'CN=USBCD' -storepass $pw -keypass $pw 2>&1 | ForEach-Object { "   $_" } | Out-Host
    $ErrorActionPreference = 'Stop'
    if (-not (Test-Path $ks)) { throw 'keytool konnte den Schlüssel nicht anlegen (Android Studio installiert?)' }
    Write-Host '   angelegt: installer\keys\ – bitte sicher aufbewahren, nicht weitergeben'
}
$env:USBCD_KEYSTORE = $ks
$env:USBCD_KEYSTORE_PASSWORD = (Get-Content -LiteralPath $pwFile -Raw).Trim()
$env:USBCD_KEY_ALIAS = 'usbcd'

Push-Location $android
$ErrorActionPreference = 'Continue'   # Gradle schreibt Fortschritt auf stderr
& .\gradlew.bat assembleRelease --console=plain
$gradleOk = $LASTEXITCODE -eq 0
$ErrorActionPreference = 'Stop'
Pop-Location
$apk = Join-Path $android 'app\build\outputs\apk\release\app-release.apk'
if ($gradleOk -and (Test-Path $apk)) {
    Copy-Item $apk "$deps\USBCD.apk" -Force
    $vc = (Select-String -Path (Join-Path $android 'app\build.gradle.kts') -Pattern 'versionCode\s*=\s*(\d+)').Matches[0].Groups[1].Value
    Set-Content "$deps\USBCD.apk.version" $vc -Encoding ASCII
    Write-Host "   App gebaut (versionCode $vc)"
} else {
    Write-Warning '   App konnte nicht gebaut werden – Setup entsteht ohne App (Android Studio öffnen und einmal synchronisieren?)'
}

# --- 6. Inno Setup ---
# Version des Inno Setup, zu dem ISCC.exe gehört (z. B. 6.7.4), aus dem Uninstall-Eintrag;
# $null, wenn sie sich nicht sicher bestimmen lässt (dann entscheidet der Compiler selbst, siehe USBCD.iss)
function Get-IsccVersion($path) {
    $dir = (Split-Path $path -Parent).TrimEnd('\')
    $keys = @('HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Inno Setup*_is1',
              'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Inno Setup*_is1',
              'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Inno Setup*_is1')
    foreach ($k in (Get-Item $keys -ErrorAction SilentlyContinue)) {
        $p = Get-ItemProperty $k.PSPath -ErrorAction SilentlyContinue
        if (-not $p.InstallLocation -or -not $p.DisplayVersion) { continue }
        if ($p.InstallLocation.TrimEnd('\') -ne $dir) { continue }
        if ($p.DisplayVersion -match '^\s*(\d+)\.(\d+)(?:\.(\d+))?') {
            return [version]::new([int]$Matches[1], [int]$Matches[2], [int]("0$($Matches[3])"))
        }
    }
    $v = (Get-Item $path).VersionInfo
    foreach ($s in @($v.ProductVersion, $v.FileVersion)) {
        if ($s -match '^\s*(\d+)\.(\d+)(?:\.(\d+))?' -and [int]$Matches[1] -ge 1) {
            return [version]::new([int]$Matches[1], [int]$Matches[2], [int]("0$($Matches[3])"))
        }
    }
    return $null
}
# neuestes installiertes ISCC.exe (Inno Setup 6 und 7 können nebeneinander installiert sein)
function Find-Iscc {
    $found = @()
    $c = Get-Command iscc.exe -ErrorAction SilentlyContinue
    if ($c) { $found += $c.Source }
    foreach ($v in 6, 7) {
        foreach ($p in @("${env:ProgramFiles(x86)}\Inno Setup $v\ISCC.exe", "$env:ProgramFiles\Inno Setup $v\ISCC.exe",
                         "$env:LOCALAPPDATA\Programs\Inno Setup $v\ISCC.exe")) {
            if (Test-Path $p) { $found += $p }
        }
    }
    if (-not $found) { return $null }
    return $found | Sort-Object { $ver = Get-IsccVersion $_; if ($ver) { $ver } else { [version]'0.0' } } -Descending |
        Select-Object -First 1
}
$iscc = Find-Iscc
if (-not $iscc) {
    Step 'Inno Setup installieren (winget)'
    winget install --id JRSoftware.InnoSetup -e --accept-package-agreements --accept-source-agreements
    $iscc = Find-Iscc
    if (-not $iscc) { throw 'ISCC.exe nicht gefunden – Inno Setup 6 von jrsoftware.org installieren' }
}
# Der Dunkelmodus des Setups braucht Inno Setup 6.6 oder neuer
function Update-InnoSetup {
    Step 'Inno Setup aktualisieren (winget)'
    winget upgrade --id JRSoftware.InnoSetup -e --accept-package-agreements --accept-source-agreements | Out-Host
    $script:iscc = Find-Iscc
    $script:isccVer = Get-IsccVersion $script:iscc
}
$isccVer = Get-IsccVersion $iscc
if ($isccVer -and $isccVer -lt [version]'6.6.0') { Update-InnoSetup }
if ($isccVer) { Write-Host "   Inno Setup $isccVer" } else { Write-Host "   Inno Setup: $iscc" }

$signArgs = @()
if ($env:USBCD_SIGN_CERT_THUMBPRINT) {
    Step 'Code-Signatur vorbereiten'
    $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $signtool) { throw 'signtool.exe nicht gefunden – Windows SDK installieren' }
    $ts = if ($env:USBCD_SIGN_TIMESTAMP_URL) { $env:USBCD_SIGN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
    # $q = Anführungszeichen, $f = zu signierende Datei (Platzhalter von Inno Setup)
    $signArgs = @('/DSign', "/Susbcd=`$q$($signtool.FullName)`$q sign /sha1 $($env:USBCD_SIGN_CERT_THUMBPRINT) /fd sha256 /tr $ts /td sha256 `$f")
    Write-Host "   signiere mit Zertifikat $($env:USBCD_SIGN_CERT_THUMBPRINT)"
}

function Invoke-Iscc {
    $ErrorActionPreference = 'Continue'   # Meldungen auf stderr sind kein Abbruch, der Exit-Code entscheidet
    # Ausgabe live anzeigen und zugleich für die Auswertung behalten
    & $iscc "/DAppVersion=$version" @signArgs (Join-Path $here 'USBCD.iss') 2>&1 | ForEach-Object { "$_" } |
        Tee-Object -Variable log | Out-Host
    $code = $LASTEXITCODE
    $script:isccLog = $log
    return $code
}
Step "Installer kompilieren (Version $version)"
$rc = Invoke-Iscc
# USBCD.iss bricht mit „Inno Setup 6.6 oder neuer nötig“ ab, wenn der Compiler zu alt ist:
# dann einmal aktualisieren und erneut kompilieren
if ($rc -ne 0 -and (($isccLog | Out-String) -match '6\.6 oder neuer')) {
    Update-InnoSetup
    Step "Installer kompilieren (Version $version, neuer Versuch)"
    $rc = Invoke-Iscc
    if ($rc -ne 0 -and (($isccLog | Out-String) -match '6\.6 oder neuer')) {
        throw 'Inno Setup ist zu alt – bitte Version 6.6 oder neuer von jrsoftware.org installieren und erneut bauen'
    }
}
# Virenscanner (meist Windows Defender) sperrt das frische Setup kurz, während Inno Setup
# Symbol und Versionsdaten hineinschreibt („EndUpdateResource failed“) – kurz warten, neu versuchen
for ($try = 2; $rc -ne 0 -and $try -le 3 -and (($isccLog | Out-String) -match 'EndUpdateResource|Resource update error'); $try++) {
    Write-Host '   Setup-Datei kurz vom Virenscanner gesperrt – neuer Versuch in 3 s'
    Start-Sleep -Seconds 3
    Step "Installer kompilieren (Version $version, Versuch $try)"
    $rc = Invoke-Iscc
}
if ($rc -ne 0) { throw 'Inno Setup meldet Fehler' }

$out = Join-Path $here "Output\USBCD-Setup-$version.exe"
# ältere Setups aufräumen – nur das gerade gebaute bleibt liegen
if (Test-Path -LiteralPath $out) {
    Get-ChildItem -LiteralPath (Join-Path $here 'Output') -Filter 'USBCD-Setup-*.exe' |
        Where-Object { $_.FullName -ne $out } |
        ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }
}
Write-Host "`nFertig: $out" -ForegroundColor Green
Stop-Transcript | Out-Null
explorer.exe (Join-Path $here 'Output')
Read-Host 'Enter zum Schließen'
