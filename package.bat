@echo off
setlocal
cd /d "%~dp0"

rem ===== HiEditor Pro 打包脚本 =====
rem 用法：package.bat
rem 前提：已编译 release 版（rebuild.bat 或 cargo build --release）
rem 产物：dist\HiEditorPro-v1.0.0.zip
rem       内含 hieditor-pro.exe + 右键菜单安装（install.bat / uninstall.bat /
rem       hieditor_menu.dll / sparse\ 稀疏包脚本，解压后运行 install.bat 即可注册）

set "VERSION=1.0.0"
set "DIST=HiEditorPro-v%VERSION%"
set "STAGE=%~dp0dist\%DIST%"

if not exist "%~dp0target\release\hieditor-pro.exe" (
    echo *** 未找到 target\release\hieditor-pro.exe，请先执行 rebuild.bat 编译 ***
    exit /b 1
)
if not exist "%~dp0target\release\hieditor_menu.dll" (
    echo *** 未找到 target\release\hieditor_menu.dll，请先执行 rebuild.bat 编译 ***
    exit /b 1
)

echo === [1/4] 准备打包目录 %DIST% ===
if exist "%STAGE%" rmdir /s /q "%STAGE%"
if exist "%~dp0dist\%DIST%.zip" del /q "%~dp0dist\%DIST%.zip"
mkdir "%STAGE%" 2>nul

echo === [2/4] 复制 hieditor-pro.exe ===
copy /y "%~dp0target\release\hieditor-pro.exe" "%STAGE%" >nul || goto :fail

echo === [3/4] 复制右键菜单安装文件 ===
copy /y "%~dp0target\release\hieditor_menu.dll" "%STAGE%" >nul || goto :fail
copy /y "%~dp0install.bat" "%STAGE%" >nul || goto :fail
copy /y "%~dp0uninstall.bat" "%STAGE%" >nul || goto :fail
mkdir "%STAGE%\sparse\assets" 2>nul
copy /y "%~dp0sparse\AppxManifest.xml" "%STAGE%\sparse\" >nul || goto :fail
copy /y "%~dp0sparse\setup-sparse.ps1" "%STAGE%\sparse\" >nul || goto :fail
copy /y "%~dp0sparse\assets\Square150x150Logo.png" "%STAGE%\sparse\assets\" >nul || goto :fail
copy /y "%~dp0sparse\assets\Square44x44Logo.png" "%STAGE%\sparse\assets\" >nul || goto :fail

echo === [4/4] 压缩为 zip ===
powershell -NoProfile -Command "Compress-Archive -Path '%STAGE%\*' -DestinationPath '%~dp0dist\%DIST%.zip' -Force"
if errorlevel 1 goto :fail

echo === 打包完成 ===
for %%A in ("%~dp0dist\%DIST%.zip") do echo   产物: %DIST%.zip ^(%%~zA 字节^)
echo   位置: %~dp0dist\%DIST%.zip
echo   内容: hieditor-pro.exe / hieditor_menu.dll / install.bat / uninstall.bat / sparse\
exit /b 0

:fail
echo.
echo *** 打包失败 ***
exit /b 1
