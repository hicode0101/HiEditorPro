# 准备稀疏 MSIX 包：校验资源、复制 exe/dll 到包根目录、注册 CLSID（HKCU，均免管理员）。
# 包根目录 = 本脚本所在 sparse/ 目录（AppxManifest.xml 所在处）。
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot                                   # sparse 目录
$parent = Split-Path -Parent $root                      # 项目根 / 发行包根

# 1) logo 资源（由 gen-assets.ps1 生成，此处仅校验）
New-Item -ItemType Directory -Force -Path (Join-Path $root 'assets') | Out-Null
foreach ($f in 'Square150x150Logo.png','Square44x44Logo.png') {
    $p = Join-Path $root "assets\$f"
    if (!(Test-Path $p)) { throw "missing $p ; run: powershell -File sparse\gen-assets.ps1" }
}
Write-Host '[1/3] assets OK'

# 2) 复制主程序 exe 与菜单组件 dll 进包根目录。
#    开发布局：..\target\release\{hieditor-pro.exe, hieditor_menu.dll}
#    发行包布局：..\{hieditor-pro.exe | hieditor-gui.exe(旧版)} + ..\hieditor_menu.dll
$exe = $null
foreach ($c in @(
    (Join-Path $parent 'target\release\hieditor-pro.exe'),
    (Join-Path $parent 'hieditor-pro.exe'),
    (Join-Path $parent 'hieditor-gui.exe'))) {
    if (Test-Path $c) { $exe = $c; break }
}
if (!$exe) { throw 'missing hieditor-pro.exe ; run rebuild.bat' }

$dll = $null
foreach ($c in @(
    (Join-Path $parent 'target\release\hieditor_menu.dll'),
    (Join-Path $parent 'hieditor_menu.dll'))) {
    if (Test-Path $c) { $dll = $c; break }
}
if (!$dll) { throw 'missing hieditor_menu.dll ; run rebuild.bat' }
Copy-Item $exe $root -Force
Copy-Item $dll $root -Force
Write-Host "[2/3] files copied OK ($((Split-Path -Leaf $exe)), hieditor_menu.dll)"

# 3) 清单 Executable 与实际 exe 名对齐（默认 hieditor-pro.exe，旧发行包可能为 hieditor-gui.exe）
$manifest = Join-Path $root 'AppxManifest.xml'
$exeName = Split-Path -Leaf $exe
if ($exeName -ne 'hieditor-pro.exe') {
    (Get-Content $manifest -Raw) -replace 'Executable="[^"]*"', "Executable=`"$exeName`"" |
        Set-Content $manifest -Encoding UTF8 -NoNewline
    Write-Host "    manifest Executable -> $exeName"
}

# 4) CLSID 注册到 HKCU（当前用户，免管理员）。
#    菜单激活走清单 com:ComServer；这份注册表项仅供 CoCreateInstance 激活自检
#    （sparse\test-activation.ps1）使用。
$clsid = '{2E7EED97-9BAE-4334-8DA1-8DE129DA7024}'
$dllPath = Join-Path $root 'hieditor_menu.dll'
$key = "HKCU:\Software\Classes\CLSID\$clsid"
New-Item -Path $key -Force | Out-Null
Set-Item -Path $key -Value 'HiEditor Pro IExplorerCommand'
New-Item -Path "$key\InprocServer32" -Force | Out-Null
Set-Item -Path "$key\InprocServer32" -Value $dllPath
New-ItemProperty -Path "$key\InprocServer32" -Name ThreadingModel -Value 'Apartment' -PropertyType String -Force | Out-Null
Write-Host "[3/3] CLSID registered -> $dllPath"
