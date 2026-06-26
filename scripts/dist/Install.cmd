@echo off
REM Double-click entry point for the unseamless-coop installer.
REM Runs Install.ps1 with -ExecutionPolicy Bypass so a downloaded, unsigned script still runs
REM (PowerShell's default policy blocks downloaded scripts; Bypass applies to this run only).
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0Install.ps1" %*
echo.
pause
