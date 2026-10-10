@echo off
rem ===== HiEditor Pro 右键菜单卸载（幂等，全部当前用户作用域，免管理员）=====
setlocal
set ROOT=%~dp0

echo [1/3] 移除稀疏包 (Win11 新菜单)...
powershell -NoProfile -Command "Get-AppxPackage -Name HiEditorProContextMenu | Remove-AppxPackage -ErrorAction SilentlyContinue"

echo [2/3] 移除传统动词...
if exist "%ROOT%target\release\hieditor-pro.exe" "%ROOT%target\release\hieditor-pro.exe" --unregister
if exist "%ROOT%hieditor-pro.exe" "%ROOT%hieditor-pro.exe" --unregister
if exist "%ROOT%hieditor-gui.exe" "%ROOT%hieditor-gui.exe" --unregister

echo [3/3] 移除 CLSID 注册...
powershell -NoProfile -Command "Remove-Item 'HKCU:\Software\Classes\CLSID\{2E7EED97-9BAE-4334-8DA1-8DE129DA7024}' -Recurse -Force -ErrorAction SilentlyContinue"

echo.
echo 完成。重启资源管理器刷新菜单：
echo   taskkill /f /im explorer.exe ^& start explorer.exe
pause
