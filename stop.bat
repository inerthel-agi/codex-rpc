@echo off
setlocal EnableExtensions
title Codex Rich Presence - stop
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0scripts\stop-daemon.ps1"
set "CODE=%ERRORLEVEL%"
echo.
pause >nul
endlocal & exit /b %CODE%
