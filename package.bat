@echo off
setlocal
cd /d "%~dp0"

rem ===== HiEditor Pro 打包脚本 =====
rem 用法：package.bat
rem 前提：已编译 release 版（rebuild.bat 或 cargo build --release）
rem 产物：dist\HiEditorPro-v1.0.0.zip（内含 hieditor-pro.exe）

set "VERSION=1.0.0"
set "DIST=HiEditorPro-v%VERSION%"
set "STAGE=%~dp0dist\%DIST%"

if not exist "%~dp0target\release\hieditor-gui.exe" (
    echo *** 未找到 target\release\hieditor-gui.exe，请先执行 rebuild.bat 编译 ***
    exit /b 1
)

echo === [1/3] 准备打包目录 %DIST% ===
if exist "%STAGE%" rmdir /s /q "%STAGE%"
if exist "%~dp0dist\%DIST%.zip" del /q "%~dp0dist\%DIST%.zip"
mkdir "%STAGE%" 2>nul

echo === [2/3] 复制 hieditor-gui.exe 为 hieditor-pro.exe ===
copy /y "%~dp0target\release\hieditor-gui.exe" "%STAGE%\hieditor-pro.exe" >nul || goto :fail

echo === [3/3] 压缩为 zip ===
powershell -NoProfile -Command "Compress-Archive -Path '%STAGE%\*' -DestinationPath '%~dp0dist\%DIST%.zip' -Force"
if errorlevel 1 goto :fail

echo === 打包完成 ===
for %%A in ("%~dp0dist\%DIST%.zip") do echo   产物: %DIST%.zip ^(%%~zA 字节^)
echo   位置: %~dp0dist\%DIST%.zip
echo   内容: hieditor-pro.exe
exit /b 0

:fail
echo.
echo *** 打包失败 ***
exit /b 1
