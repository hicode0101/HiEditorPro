@echo off
setlocal enabledelayedexpansion
cd /d "%~dp0"

rem ===== HiEditor Pro 发布打包脚本 =====
rem 用法：release.bat
rem 产物：dist\HiEditorPro-v版本.zip（含 gui/cli 可执行文件与 README）

where cargo >nul 2>nul
if errorlevel 1 set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

rem --- 若程序正在运行则无法覆盖 exe ---
tasklist 2>nul | findstr /i "hieditor-gui.exe" >nul
if not errorlevel 1 (
    echo *** 检测到 HiEditor Pro 正在运行，请先关闭后再打包 ***
    exit /b 1
)

echo === [1/4] cargo build --release --workspace ===
cargo build --release --workspace
if errorlevel 1 goto :fail

rem --- 从 Cargo.toml 提取版本号（PowerShell 输出纯版本号）---
set "VERSION=0.1.0"
for /f "usebackq delims=" %%V in (`powershell -NoProfile -Command "(Select-String -Path Cargo.toml -Pattern '^version' ^| Select-Object -First 1).Line -replace '[^0-9.]',''" 2^>nul`) do set "VERSION=%%V"
set "DIST=HiEditorPro-v%VERSION%"
set "STAGE=%~dp0dist\%DIST%"

echo === [2/4] 准备发布目录 %DIST% ===
if exist "%STAGE%" rmdir /s /q "%STAGE%"
if exist "%~dp0dist\%DIST%.zip" del /q "%~dp0dist\%DIST%.zip"
mkdir "%STAGE%" 2>nul

copy /y "%~dp0target\release\hieditor-gui.exe" "%STAGE%" >nul || goto :fail
copy /y "%~dp0target\release\hieditor-cli.exe" "%STAGE%" >nul || goto :fail
copy /y "%~dp0README.md" "%STAGE%" >nul || goto :fail

echo === [3/4] 压缩为 zip ===
powershell -NoProfile -Command "Compress-Archive -Path '%STAGE%\*' -DestinationPath '%~dp0dist\%DIST%.zip' -Force"
if errorlevel 1 goto :fail

echo === [4/4] 完成 ===
for %%A in ("%~dp0dist\%DIST%.zip") do echo   产物: %DIST%.zip ^(%%~zA 字节^)
echo   位置: %~dp0dist\%DIST%.zip
echo   内容: hieditor-gui.exe / hieditor-cli.exe / README.md
exit /b 0

:fail
echo.
echo *** 发布打包失败 ***
exit /b 1
