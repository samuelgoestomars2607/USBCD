# USBCD – Diagnose (Startmenü → „USBCD – Diagnose“). Läuft ohne Administratorrechte
# und schreibt alles nach %LOCALAPPDATA%\USBCD\diagnose.txt.
$ErrorActionPreference = 'Continue'
$data = Join-Path $env:LOCALAPPDATA 'USBCD'
New-Item -ItemType Directory -Force -Path $data | Out-Null
$out = Join-Path $data 'diagnose.txt'
$app = Split-Path -Parent $PSCommandPath
Start-Transcript -Path $out -Force | Out-Null

Write-Output "`n===== USBCD"
Get-Item "$app\USBCD.exe" -ErrorAction SilentlyContinue | Format-List FullName, Length, LastWriteTime | Out-String
Get-Process USBCD -ErrorAction SilentlyContinue | Format-List Id, Path, StartTime | Out-String

Write-Output "`n===== Hilfsdienst"
Get-Service USBCDHelper -ErrorAction SilentlyContinue | Format-List Name, Status, StartType | Out-String
Get-Content "$env:ProgramData\USBCD\helper.log" -Tail 30 -ErrorAction SilentlyContinue

Write-Output "`n===== Autostart"
(Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name USBCD -ErrorAction SilentlyContinue).USBCD

Write-Output "`n===== Monitor-Treiber"
Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { ($_.HardwareID -join ';') -match 'MttVDD' } |
    Format-List FriendlyName, Status, Class, InstanceId, Problem | Out-String
Write-Output "Parsec (soll unverändert sein):"
Get-PnpDevice -FriendlyName '*Parsec*' -ErrorAction SilentlyContinue | Format-Table FriendlyName, Status -AutoSize | Out-String

Write-Output "`n===== Audio-Geräte"
Get-PnpDevice -Class AudioEndpoint -ErrorAction SilentlyContinue | Format-Table FriendlyName, Status -AutoSize | Out-String

Write-Output "`n===== vdd_settings.xml"
Get-Content C:\VirtualDisplayDriver\vdd_settings.xml -TotalCount 30 -ErrorAction SilentlyContinue
icacls C:\VirtualDisplayDriver 2>&1 | Out-String

Write-Output "`n===== Webcam"
Get-Item "$app\usbcd_cam.dll" -ErrorAction SilentlyContinue | Format-List FullName, Length, LastWriteTime | Out-String
(Get-ItemProperty 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\CLSID\{CE63EC35-A188-4E54-8E6F-8F3C01EF1CBA}\InprocServer32' -ErrorAction SilentlyContinue).'(default)'
Get-Service FrameServer -ErrorAction SilentlyContinue | Format-List Name, Status, StartType | Out-String
Get-PnpDevice -Class Camera -ErrorAction SilentlyContinue | Format-Table FriendlyName, Status -AutoSize | Out-String
Get-Content "$env:ProgramData\USBCD\kamera.log" -Tail 20 -ErrorAction SilentlyContinue

Write-Output "`n===== adb"
& "$app\platform-tools\adb.exe" devices -l 2>&1 | Out-String

Write-Output "`n===== ffmpeg"
& "$app\ffmpeg.exe" -hide_banner -h filter=gfxcapture 2>&1 | Select-Object -First 4 | Out-String

Write-Output "`n===== Log"
Get-Content (Join-Path $data 'usbcd.log') -Tail 80 -ErrorAction SilentlyContinue
Stop-Transcript | Out-Null
Start-Process notepad.exe $out
