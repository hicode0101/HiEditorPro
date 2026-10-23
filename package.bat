@echo off
setlocal
cd /d "%~dp0"

rem ===== HiEditor Pro package script =====
rem Usage: package.bat
rem Prereq: release build done (rebuild.bat or cargo build --release)
rem Output: dist\HiEditorPro-v1.0.0.zip
rem         Contains HiEditorPro.exe + context-menu registration files
rem         (hieditor_menu.dll / sparse\ signed package). After extracting,
rem         run "HiEditorPro --register" or use GUI Settings to register.

set "VERSION=1.0.0"
set "DIST=HiEditorPro-v%VERSION%"
set "STAGE=%~dp0dist\%DIST%"

if not exist "%~dp0target\release\HiEditorPro.exe" (
    echo *** target\release\HiEditorPro.exe not found, run rebuild.bat first ***
    exit /b 1
)
if not exist "%~dp0target\release\hieditor_menu.dll" (
    echo *** target\release\hieditor_menu.dll not found, run rebuild.bat first ***
    exit /b 1
)

echo === [1/5] Prepare stage dir %DIST% ===
if exist "%STAGE%" rmdir /s /q "%STAGE%"
if exist "%~dp0dist\%DIST%.zip" del /q "%~dp0dist\%DIST%.zip"
mkdir "%STAGE%" 2>nul

echo === [2/5] Copy HiEditorPro.exe and README ===
copy /y "%~dp0target\release\HiEditorPro.exe" "%STAGE%" >nul || goto :fail
copy /y "%~dp0README.md" "%STAGE%" >nul || goto :fail

echo === [3/5] Copy context-menu registration files ===
copy /y "%~dp0target\release\hieditor_menu.dll" "%STAGE%" >nul || goto :fail
mkdir "%STAGE%\sparse\assets" 2>nul
copy /y "%~dp0sparse\AppxManifest.xml" "%STAGE%\sparse\" >nul || goto :fail
copy /y "%~dp0sparse\assets\Square150x150Logo.png" "%STAGE%\sparse\assets\" >nul || goto :fail
copy /y "%~dp0sparse\assets\Square44x44Logo.png" "%STAGE%\sparse\assets\" >nul || goto :fail

echo === [4/5] Build signed sparse package (no Developer Mode needed) ===
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0signing\build-msix.ps1" -ManifestDir "%STAGE%\sparse" -OutMsix "%STAGE%\sparse\HiEditorProContextMenu.msix" -CertOut "%STAGE%\sparse\HiEditorPro.cer"
if errorlevel 1 echo *** WARN: signed package build failed (missing signing\hieditor-signing.pfx or Windows SDK), fallback to unsigned manifest (needs Developer Mode) ***

echo === [5/5] Compress to zip ===
powershell -NoProfile -Command "Compress-Archive -Path '%STAGE%\*' -DestinationPath '%~dp0dist\%DIST%.zip' -Force"
if errorlevel 1 goto :fail

echo === Package done ===
for %%A in ("%~dp0dist\%DIST%.zip") do echo   Output: %DIST%.zip ^(%%~zA bytes^)
echo   Location: %~dp0dist\%DIST%.zip
echo   Contents: HiEditorPro.exe / hieditor_menu.dll / README.md / sparse\
echo   Register: run HiEditorPro --register (confirm UAC once for cert import)
exit /b 0

:fail
echo.
echo *** Package failed ***
exit /b 1
