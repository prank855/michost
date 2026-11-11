@echo off
setlocal
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
set "code=%errorlevel%"
if not "%MICHOST_NO_PAUSE%"=="1" pause
exit /b %code%
