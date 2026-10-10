@echo off
setlocal
cd /d "%~dp0"

rem ===== HiEditor Pro 重新编译脚本 =====
rem 用法：
rem   rebuild.bat          编译（release）
rem   rebuild.bat test     编译 + 运行全部测试
rem
rem 首次在新机器编译需要联网拉取依赖时，取消下面两行注释：
rem set "HTTP_PROXY=http://127.0.0.1:7891"
rem set "HTTPS_PROXY=http://127.0.0.1:7891"

where cargo >nul 2>nul
if errorlevel 1 set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

echo === [1/2] cargo build --release --workspace ===
cargo build --release --workspace
if errorlevel 1 goto :fail

if /i "%~1"=="test" (
    echo === [2/2] cargo test --workspace ===
    cargo test --workspace
    if errorlevel 1 goto :fail
) else (
    echo === [2/2] 跳过测试 ===
    echo     需要连测试一起跑请执行: rebuild.bat test
)

echo.
echo === 编译完成 ===
echo   CLI : %~dp0target\release\hieditor-cli.exe
echo   GUI : %~dp0target\release\hieditor-pro.exe
exit /b 0

:fail
echo.
echo *** 编译或测试失败 ***
exit /b 1
