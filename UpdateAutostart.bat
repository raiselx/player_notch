@echo off
setlocal
if not exist "%~dp0NotchPlayer.exe" (
    echo NotchPlayer.exe was not found next to this script.
    pause
    exit /b 1
)
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v "FloatingNotchPlayer" /t REG_SZ /d "\"%~dp0NotchPlayer.exe\" --autostart" /f >nul
if errorlevel 1 (
    echo Failed to update autostart.
    pause
    exit /b 1
)
echo Autostart now points to: %~dp0NotchPlayer.exe
pause
