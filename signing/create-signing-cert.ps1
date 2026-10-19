# ===== 生成 HiEditor Pro 自签代码签名证书（一次性） =====
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File signing\create-signing-cert.ps1
# 产出：signing\hieditor-signing.pfx（私钥，已 gitignore，勿入库/外传）
#       signing\hieditor-signing.cer（公钥证书，随发行包分发用于信任导入）
# 证书 Subject CN=HiEditorPro 必须与 sparse\AppxManifest.xml 的 Identity@Publisher 一致

$ErrorActionPreference = 'Stop'

$existing = Get-ChildItem Cert:\CurrentUser\My |
    Where-Object { $_.Subject -eq 'CN=HiEditorPro' -and $_.HasPrivateKey } |
    Sort-Object NotAfter -Descending | Select-Object -First 1
if ($existing) {
    Write-Output "证书已存在，跳过生成：$($existing.Thumbprint)（有效至 $($existing.NotAfter)）"
    $cert = $existing
} else {
    $cert = New-SelfSignedCertificate `
        -Type Custom `
        -Subject "CN=HiEditorPro" `
        -KeyUsage DigitalSignature -KeyAlgorithm RSA -KeyLength 2048 `
        -FriendlyName "HiEditor Pro Package Signing" `
        -CertStoreLocation "Cert:\CurrentUser\My" `
        -NotAfter (Get-Date).AddYears(10) `
        -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
    Write-Output "已生成证书：$($cert.Thumbprint)（有效至 $($cert.NotAfter)）"
}

$password = ConvertTo-SecureString -String 'hieditor-signing' -Force -AsPlainText
Export-PfxCertificate -Cert $cert -FilePath (Join-Path $PSScriptRoot 'hieditor-signing.pfx') -Password $password | Out-Null
Export-Certificate -Cert $cert -FilePath (Join-Path $PSScriptRoot 'hieditor-signing.cer') | Out-Null
Write-Output "已导出：$PSScriptRoot\hieditor-signing.pfx"
Write-Output "已导出：$PSScriptRoot\hieditor-signing.cer"
