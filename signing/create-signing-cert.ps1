# ===== 初始化固定签名证书（仅首次：缺少 pfx 时生成） =====
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File signing\create-signing-cert.ps1
# 固定证书策略：signing\hieditor-signing.pfx 是唯一证书来源（已 gitignore，勿入库/外传），
# 本地与 CI（经 GitHub Secret SIGNING_PFX 注入同一文件）使用同一张证书，版本间保持不变。
# 产出：signing\hieditor-signing.pfx（私钥）；signing\hieditor-signing.cer（公钥，随发行包分发）
# 已有 pfx 时不再生成（与机器证书库无关）；需要更换证书请运行 signing\reset-signing-cert.ps1
# 证书 Subject CN=HiEditorPro 必须与 sparse\AppxManifest.xml 的 Identity@Publisher 一致

$ErrorActionPreference = 'Stop'
$pfx = Join-Path $PSScriptRoot 'hieditor-signing.pfx'
$cer = Join-Path $PSScriptRoot 'hieditor-signing.cer'
if (Test-Path $pfx) {
    Write-Output "固定签名证书已存在：$pfx（如需更换请运行 signing\reset-signing-cert.ps1）"
    exit 0
}

$cert = New-SelfSignedCertificate `
    -Type Custom `
    -Subject "CN=HiEditorPro" `
    -KeyUsage DigitalSignature -KeyAlgorithm RSA -KeyLength 2048 `
    -FriendlyName "HiEditor Pro Package Signing" `
    -CertStoreLocation "Cert:\CurrentUser\My" `
    -NotAfter (Get-Date).AddYears(10) `
    -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
Write-Output "已生成证书：$($cert.Thumbprint)（有效至 $($cert.NotAfter)）"

$password = ConvertTo-SecureString -String 'hieditor-signing' -Force -AsPlainText
Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $password | Out-Null
Export-Certificate -Cert $cert -FilePath $cer | Out-Null
Write-Output "已导出：$pfx"
Write-Output "已导出：$cer"
Write-Output "请把 $pfx 的 Base64 更新到 GitHub Secret SIGNING_PFX，供 CI 打包使用"
