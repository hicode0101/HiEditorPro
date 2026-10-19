@echo off
setlocal enabledelayedexpansion
cd /d "%~dp0"

rem ===== HiEditor Pro release packaging script =====
rem Usage: release.bat
rem Output: dist\HiEditorPro-v<version>.zip (gui/cli executables + README
rem         + context-menu registration files)

where cargo >nul 2>nul
if errorlevel 1 set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

rem --- abort if the app is running (exe cannot be overwritten) ---
tasklist 2>nul | findstr /i "hieditor-pro.exe" >nul
if not errorlevel 1 (
    echo *** HiEditor Pro is running, please close it before packaging ***
    exit /b 1
)

echo === [1/4] cargo build --release --workspace ===
cargo build --release --workspace
if errorlevel 1 goto :fail

rem --- extract version from Cargo.toml ---
set "VERSION=0.1.0"
for /f "usebackq delims=" %%V in (`powershell -NoProfile -Command "(Select-String -Path Cargo.toml -Pattern '^version' ^| Select-Object -First 1).Line -replace '[^0-9.]',''" 2^>nul`) do set "VERSION=%%V"
set "DIST=HiEditorPro-v%VERSION%"
set "STAGE=%~dp0dist\%DIST%"

echo === [2/4] Prepare stage dir %DIST% ===
if exist "%STAGE%" rmdir /s /q "%STAGE%"
if exist "%~dp0dist\%DIST%.zip" del /q "%~dp0dist\%DIST%.zip"
mkdir "%STAGE%" 2>nul

copy /y "%~dp0target\release\hieditor-pro.exe" "%STAGE%" >nul || goto :fail
copy /y "%~dp0target\release\hieditor-cli.exe" "%STAGE%" >nul || goto :fail
copy /y "%~dp0README.md" "%STAGE%" >nul || goto :fail

rem --- context-menu registration files (hieditor-pro --register:
rem     classic verb + Win11 signed sparse package) ---
copy /y "%~dp0target\release\hieditor_menu.dll" "%STAGE%" >nul || goto :fail
mkdir "%STAGE%\sparse\assets" 2>nul
copy /y "%~dp0sparse\AppxManifest.xml" "%STAGE%\sparse\" >nul || goto :fail
copy /y "%~dp0sparse\assets\Square150x150Logo.png" "%STAGE%\sparse\assets\" >nul || goto :fail
copy /y "%~dp0sparse\assets\Square44x44Logo.png" "%STAGE%\sparse\assets\" >nul || goto :fail

rem --- build signed sparse package (warn only on failure: falls back to
rem     unsigned manifest, registration then needs Developer Mode) ---
echo === Build: signed sparse package ===
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0signing\build-msix.ps1" -ManifestDir "%STAGE%\sparse" -OutMsix "%STAGE%\sparse\HiEditorProContextMenu.msix" -CertOut "%STAGE%\sparse\HiEditorPro.cer"
if errorlevel 1 echo *** WARN: signed package build failed (missing signing\hieditor-signing.pfx or Windows SDK), fallback to unsigned manifest ***

echo === [3/4] Compress to zip ===
powershell -NoProfile -Command "Compress-Archive -Path '%STAGE%\*' -DestinationPath '%~dp0dist\%DIST%.zip' -Force"
if errorlevel 1 goto :fail

echo === [4/4] Done ===
for %%A in ("%~dp0dist\%DIST%.zip") do echo   Output: %DIST%.zip ^(%%~zA bytes^)
echo   Location: %~dp0dist\%DIST%.zip
echo   Contents: hieditor-pro.exe / hieditor-cli.exe / README.md / context-menu files
echo   Register: run hieditor-pro --register (confirm UAC once for cert import)
exit /b 0

:fail
echo.
echo *** Release packaging failed ***
exit /b 1
