@echo off
setlocal
cd /d "%~dp0"

rem ------------------------------------------------------------------
rem Build the dist (size-optimized) exe and collect it into dist\.
rem
rem   cargo build --profile dist   ->  target\dist\RunAny_RS.exe
rem   dist\RunAny_RS.exe           ->  the file we ship
rem
rem NOTE: no UPX. Packing the exe made every build take minutes and some
rem antivirus scanners flag it; the dist profile alone is small enough.
rem ------------------------------------------------------------------

where cargo >nul 2>nul || set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

echo.
echo   [1/3] cargo build --profile dist
echo.
cargo build --profile dist
if errorlevel 1 goto :fail

echo.
echo   [2/3] collecting into dist\
echo.
if not exist dist mkdir dist
copy /y target\dist\RunAny_RS.exe dist\RunAny_RS.exe >nul
if errorlevel 1 goto :fail

rem Everything SDK (x64). Without it the index search silently degrades.
if exist third_party\everything-sdk\Everything64.dll (
  copy /y third_party\everything-sdk\Everything64.dll dist\Everything64.dll >nul
  echo   Everything64.dll copied
)

rem Strip anything that belongs to *this* machine: the exe recreates it on
rem first run, and shipping it would overwrite the user's own config.
if exist dist\settings.json del /q dist\settings.json
if exist dist\programs.json del /q dist\programs.json
if exist dist\menus rmdir /s /q dist\menus
if exist dist\Icon\icons.tsv del /q dist\Icon\icons.tsv
if exist dist\Icon\*.png del /q dist\Icon\*.png

echo.
echo   [3/3] done
echo.
dir dist\RunAny_RS.exe
endlocal
exit /b 0

:fail
echo.
echo   build failed
pause
endlocal
exit /b 1
