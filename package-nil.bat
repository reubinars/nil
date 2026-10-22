@echo off
rem Builds nil and refreshes the ready-to-use folder (see package-nil.ps1).
rem Close nil first so nil.exe can be replaced.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0package-nil.ps1"
if errorlevel 1 (
    echo.
    pause
    exit /b 1
)
