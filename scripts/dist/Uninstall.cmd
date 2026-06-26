@echo off
REM Double-click entry point to restore your original ELDEN RING setup.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0Uninstall.ps1" %*
echo.
pause
