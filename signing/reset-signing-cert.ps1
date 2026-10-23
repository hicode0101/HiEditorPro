# ===== 重置签名证书：生成新证书，覆盖 signing\hieditor-signing.pfx / .cer =====
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File signing\reset-signing-cert.ps1 [-Force]
# 注意：旧证书随之作废——已注册旧版的机器升级到用新证书签的版本时，会再弹一次 UAC 导入新 .cer。
# 覆盖后需同步两处：1) 提交新的 signing\hieditor-signing.cer
#                   2) 把新 pfx 的 Base64 更新到 GitHub Secret SIGNING_PFX

param([switch] $Force)

$ErrorActionPreference = 'Stop'
$pfx = Join-Path $PSScriptRoot 'hieditor-signing.pfx'
$cer = Join-Path $PSScriptRoot 'hieditor-signing.cer'

$old = $null
if (Test-Path $pfx) {
    $old = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($pfx, 'hieditor-signing')
}
if ($old -and -not $Force) {
    $reply = Read-Host "将覆盖现有证书 $($old.Thumbprint)（旧证书作废），继续? (y/N)"
    if ($reply -ne 'y') { Write-Output '已取消'; exit 1 }
}

$cert = New-SelfSignedCertificate `
    -Type Custom `
    -Subject "CN=HiEditorPro" `
    -KeyUsage DigitalSignature -KeyAlgorithm RSA -KeyLength 2048 `
    -FriendlyName "HiEditor Pro Package Signing" `
    -CertStoreLocation "Cert:\CurrentUser\My" `
    -NotAfter (Get-Date).AddYears(10) `
    -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
Write-Output "已生成新证书：$($cert.Thumbprint)（有效至 $($cert.NotAfter)）"

$password = ConvertTo-SecureString -String 'hieditor-signing' -Force -AsPlainText
Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $password | Out-Null
Export-Certificate -Cert $cert -FilePath $cer | Out-Null
Write-Output "已覆盖：$pfx"
Write-Output "已覆盖：$cer"
Write-Output "请同步：提交新的 $cer，并把 $pfx 的 Base64 更新到 GitHub Secret SIGNING_PFX"
