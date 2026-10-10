# 生成 sparse 包所需的 logo PNG（从 GUI 内置 logo.png 等比缩放，透明背景）
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$src = Join-Path $PSScriptRoot '..\crates\hieditor-gui\assets\logo.png'
$outDir = Join-Path $PSScriptRoot 'assets'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$img = [System.Drawing.Image]::FromFile($src)
Write-Output ("logo.png: {0}x{1}" -f $img.Width, $img.Height)

foreach ($size in 44, 150) {
    $name = if ($size -eq 44) { 'Square44x44Logo.png' } else { 'Square150x150Logo.png' }
    $bmp = New-Object System.Drawing.Bitmap($size, $size)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    # 等比缩放并居中
    $scale = [Math]::Min($size / $img.Width, $size / $img.Height)
    $w = [int]($img.Width * $scale)
    $h = [int]($img.Height * $scale)
    $x = [int](($size - $w) / 2)
    $y = [int](($size - $h) / 2)
    $g.DrawImage($img, $x, $y, $w, $h)
    $g.Dispose()
    $bmp.Save((Join-Path $outDir $name), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Output ("generated {0}" -f $name)
}
$img.Dispose()
