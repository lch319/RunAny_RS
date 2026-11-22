@echo off
setlocal
cd /d "%~dp0"

rem ------------------------------------------------------------------
rem 一键打 release 包：编译 -> UPX 压缩 -> 输出可发布的 exe
rem
rem 产物：
rem   target\release\RunAny_RS.exe      未压缩（杀软不敏感）
rem   dist\RunAny_RS.exe                压缩版（约 1.4 MB，默认发布这个）
rem
rem 依赖：UPX（https://github.com/upx/upx/releases 的 win64 zip，
rem         解压出 upx.exe 后放进本目录或 %USERPROFILE%\.local\bin 即可）
rem ------------------------------------------------------------------

where cargo >nul 2>nul || set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

rem 找 upx：本目录 -> 用户 bin -> PATH
rem 变量名不能叫 UPX：upx.exe 会把环境变量 UPX 当成默认参数，
rem 于是报 "invalid string ... in environment variable 'UPX'" 直接失败，
rem 打包脚本就会悄悄退回去发未压缩的 4.6 MB 版本。
set "UPX_EXE="
if exist "%~dp0upx.exe" set "UPX_EXE=%~dp0upx.exe"
if not defined UPX_EXE if exist "%USERPROFILE%\.local\bin\upx.exe" set "UPX_EXE=%USERPROFILE%\.local\bin\upx.exe"
if not defined UPX_EXE (
  where upx >nul 2>nul && set "UPX_EXE=upx.exe"
)

echo.
echo   [1/4] cargo build --release
echo.
cargo build --release
if errorlevel 1 goto :fail

if not defined UPX_EXE (
  echo.
  echo   [2/4] 没找到 UPX，跳过压缩（直接拷贝未压缩版）
  echo.
) else (
  echo.
  echo   [2/4] UPX 压缩（--best --lzma）
  echo.
  rem 先删掉上一次的压缩产物：UPX 遇到已存在的输出会直接拒绝
  rem （"File already exists"），以前第二次运行就会走进下面的失败分支，
  rem 然后把**未压缩**的 4.6 MB 版本拷进 dist —— 这就是「体积怎么又变回去了」的原因。
  if exist target\release\RunAny_RS-upx.exe del /q target\release\RunAny_RS-upx.exe
  "%UPX_EXE%" --best --lzma -o target\release\RunAny_RS-upx.exe target\release\RunAny_RS.exe
  if errorlevel 1 (
    echo.
    echo   ！UPX 失败（可能是杀软拦了）—— 这次只能发未压缩版
    if exist target\release\RunAny_RS-upx.exe del /q target\release\RunAny_RS-upx.exe
  )
)

echo.
echo   [3/4] 拷贝到 dist\
echo.
if not exist dist mkdir dist
if exist target\release\RunAny_RS-upx.exe (
  copy /y target\release\RunAny_RS-upx.exe dist\RunAny_RS.exe >nul
) else (
  copy /y target\release\RunAny_RS.exe dist\RunAny_RS.exe >nul
)

rem Everything SDK 的 x64 动态库（约 90 KB）：和 exe 放一起就能用它查全盘文件名。
rem 没放也能跑 —— 只是「本地索引里没有的名字」查不到完整路径。
if exist third_party\everything-sdk\Everything64.dll (
  copy /y third_party\everything-sdk\Everything64.dll dist\Everything64.dll >nul
  echo   附带 Everything64.dll（Everything 联动）
)

rem 热字符串调试启动器：带上调试开关启动，日志写在 exe 旁边的 hotstring-debug.log
if exist third_party\debug-hotstring.bat (
  copy /y third_party\debug-hotstring.bat dist\debug-hotstring.bat >nul
  echo   调试启动器 debug-hotstring.bat
)

rem 配置一个都不带：settings.json / programs.json / menus\ 都是**用户的**东西。
rem 跟着 exe 一起发出去，第一次启动就会把人家的配色、快捷键、菜单覆盖掉。
rem dist 里出现这些多半是「拿 dist 里的 exe 试跑过」，顺手清掉。
if exist dist\settings.json del /q dist\settings.json
if exist dist\programs.json del /q dist\programs.json
if exist dist\menus rmdir /s /q dist\menus
rem icon cache (exe dir\Icon\*.png + icons.tsv) is rebuilt on first run; never ship it
rem keep Icon\badges\ (user's own icons, referenced by relative path)
if exist dist\Icon\icons.tsv del /q dist\Icon\icons.tsv
if exist dist\Icon\*.png del /q dist\Icon\*.png
echo   已清掉试跑留下的配置（exe 自己会在首次启动时生成）

echo.
echo   [4/4] 打便携包（只装 exe，不带任何配置）
if exist dist\RunAny_RS_portable.zip del /q dist\RunAny_RS_portable.zip
if exist dist\Everything64.dll (
  powershell -NoProfile -Command "Compress-Archive -Path 'dist\RunAny_RS.exe','dist\Everything64.dll','dist\debug-hotstring.bat' -DestinationPath 'dist\RunAny_RS_portable.zip' -Force" >nul
) else (
  powershell -NoProfile -Command "Compress-Archive -Path 'dist\RunAny_RS.exe' -DestinationPath 'dist\RunAny_RS_portable.zip' -Force" >nul
)
if exist dist\RunAny_RS_portable.zip (
  echo   dist\RunAny_RS_portable.zip
) else (
  echo   ！便携包没打成（系统自带的 powershell 不可用？），exe 本身没问题
)

echo.
echo   完成。dist\RunAny_RS.exe
dir dist\RunAny_RS.exe
rem 体积守卫：超过 3 MB 基本就是「没压上」—— 直接说出来，别让人自己发现
for %%A in (dist\RunAny_RS.exe) do if %%~zA GTR 3145728 (
  echo.
  echo   ！！警告：dist\RunAny_RS.exe 有 %%~zA 字节，看起来是未压缩版
  echo      正常压缩后约 150 万字节。检查 upx 是否被拦截，或手动执行：
  echo      upx --best --lzma -o dist\RunAny_RS.exe target\release\RunAny_RS-upx.exe
)
endlocal
exit /b 0

:fail
echo.
echo   编译失败，看上面的报错。
pause
endlocal
exit /b 1
