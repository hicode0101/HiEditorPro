# ===== 构建并签名稀疏包（HiEditorProContextMenu.msix） =====
# 用法（release.bat / package.bat 调用，也可手动执行）：
#   powershell -NoProfile -ExecutionPolicy Bypass -File signing\build-msix.ps1 `
#       -ManifestDir <含 AppxManifest.xml 与 assets\ 的目录> `
#       -OutMsix <输出 .msix 路径> -CertOut <输出 .cer 路径>
# 依赖：Windows SDK 的 makeappx.exe / signtool.exe；signing\hieditor-signing.pfx
#       （缺失时先运行 signing\create-signing-cert.ps1）

param(
    [Parameter(Mandatory = $true)] [string] $ManifestDir,
    [Parameter(Mandatory = $true)] [string] $OutMsix,
    [Parameter(Mandatory = $true)] [string] $CertOut
)

$ErrorActionPreference = 'Stop'
$pfx = Join-Path $PSScriptRoot 'hieditor-signing.pfx'
$cer = Join-Path $PSScriptRoot 'hieditor-signing.cer'
if (-not (Test-Path $pfx)) {
    throw "缺少签名证书 $pfx，请先运行 signing\create-signing-cert.ps1"
}

# 定位 Windows SDK 工具（取版本号最高的 x64 目录）
$bin = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match '^\d' -and (Test-Path (Join-Path $_.FullName 'x64\makeappx.exe')) } |
    Sort-Object Name -Descending | Select-Object -First 1
if (-not $bin) { throw '未找到 Windows SDK（makeappx.exe）' }
$bin = Join-Path $bin.FullName 'x64'

# 干净暂存目录：只打清单与图标（exe/dll 在 ExternalLocation，不进包）
$stage = Join-Path $env:TEMP ("hieditor-msix-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    Copy-Item (Join-Path $ManifestDir 'AppxManifest.xml') $stage
    if (Test-Path (Join-Path $ManifestDir 'assets')) {
        Copy-Item (Join-Path $ManifestDir 'assets') $stage -Recurse
    }
    # /nv 跳过清单引用文件检查（exe/dll 不在包内）
    & (Join-Path $bin 'makeappx.exe') pack /o /nv /d $stage /p $OutMsix | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "makeappx 打包失败（exit $LASTEXITCODE）" }

    & (Join-Path $bin 'signtool.exe') sign /fd SHA256 /f $pfx /p 'hieditor-signing' $OutMsix | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "signtool 签名失败（exit $LASTEXITCODE）" }

    Copy-Item $cer $CertOut
    Write-Output "已生成签名稀疏包：$OutMsix"
} finally {
    Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
}
