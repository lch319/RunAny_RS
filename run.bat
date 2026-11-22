@echo off
setlocal
cd /d "%~dp0"

rem Make sure cargo is reachable even if this window started before rustup updated PATH
where cargo >nul 2>nul || set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

echo.
echo   RunAny_RS — popup menu demo
echo   hotkey: Ctrl + Alt + M   (works in any app)
echo   close the console window to quit
echo.

cargo run
if errorlevel 1 (
  echo.
  echo Build or run failed. See the messages above.
  pause
)
endlocal
