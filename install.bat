@echo off
rem ===== HiEditor Pro 右键菜单安装（幂等、免管理员）=====
rem 1) 传统动词（HKCU）：Win10 菜单第一层 / Win11「显示更多选项」
rem 2) 稀疏 MSIX 包：Win11 新版菜单第一层（需系统开启开发者模式）
setlocal
set ROOT=%~dp0

rem --- 定位主程序 exe（开发布局 target\release\；发行包根目录；旧版为 hieditor-gui.exe）---
set "EXE="
if exist "%ROOT%target\release\hieditor-pro.exe" set "EXE=%ROOT%target\release\hieditor-pro.exe"
if not defined EXE if exist "%ROOT%hieditor-pro.exe" set "EXE=%ROOT%hieditor-pro.exe"
if not defined EXE if exist "%ROOT%hieditor-gui.exe" set "EXE=%ROOT%hieditor-gui.exe"
if not defined EXE (
  echo [ERROR] 未找到 hieditor-pro.exe，请先执行 rebuild.bat
  pause
  exit /b 1
)
if not exist "%ROOT%target\release\hieditor_menu.dll" if not exist "%ROOT%hieditor_menu.dll" (
  echo [ERROR] 未找到 hieditor_menu.dll，请先执行 rebuild.bat
  pause
  exit /b 1
)
if not exist "%ROOT%sparse\AppxManifest.xml" (
  echo [ERROR] 未找到 sparse\AppxManifest.xml，安装包不完整
  pause
  exit /b 1
)

echo [1/3] 传统动词注册 (HKCU，Win10 菜单第一层 / Win11 显示更多选项)...
"%EXE%" --register

echo [2/3] 准备稀疏包文件...
powershell -NoProfile -ExecutionPolicy Bypass -File "%ROOT%sparse\setup-sparse.ps1"
if errorlevel 1 (
  echo *** 稀疏包准备失败 ***
  pause
  exit /b 1
)

echo [3/3] 注册稀疏包 (Win11 新菜单第一层；同版本先移除再注册，保证可重复执行)...
powershell -NoProfile -Command "$ErrorActionPreference='Stop'; Get-AppxPackage -Name HiEditorProContextMenu | Remove-AppxPackage -ErrorAction SilentlyContinue; Add-AppxPackage -Register '%ROOT%sparse\AppxManifest.xml'"
if errorlevel 1 (
  echo *** 稀疏包注册失败：请确认系统已开启开发者模式（设置 -^> 系统 -^> 开发者选项）***
  echo *** 失败详情可查事件日志：Microsoft-Windows-AppXDeploymentServer/Operational ***
  pause
  exit /b 1
)

echo.
echo 完成！如果菜单项未出现，重启资源管理器后重试：
echo   taskkill /f /im explorer.exe ^& start explorer.exe
echo 人工验证：右键任意文件，看 Win11 新菜单第一层是否出现「用 HiEditor Pro 打开」
pause
