@echo off
rem Doppelklick: baut installer\Output\USBCD-Setup-<version>.exe
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-installer.ps1" %*
