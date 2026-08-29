# 锚点定位可视化校验：计算锚点屏幕坐标 → 跑探测器点击 → 连拍三张局部截图。
# 用法：click-verify.ps1 -Hwnd 395386 -X 0.394 -Y 0.787 -OutDir <dir>
param(
    [long]$Hwnd,
    [double]$X = 0.394,
    [double]$Y = 0.787,
    [string]$OutDir = "$env:TEMP\anchor-verify"
)

Add-Type -Path (Join-Path $PSScriptRoot 'WEnum.cs')
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
[WEnum]::SetProcessDPIAware() | Out-Null

$h = [IntPtr]$Hwnd
$rect = New-Object WEnum+RECT
[WEnum]::GetClientRect($h, [ref]$rect) | Out-Null
$clientW = $rect.R - $rect.L
$clientH = $rect.B - $rect.T
$pt = New-Object WEnum+POINT
$pt.X = [int][Math]::Round($clientW * $X)
$pt.Y = [int][Math]::Round($clientH * $Y)
[WEnum]::ClientToScreen($h, [ref]$pt) | Out-Null
Write-Host ("client={0}x{1} anchor_screen=({2},{3})" -f $clientW, $clientH, $pt.X, $pt.Y)

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# 1) 点击前的基线截图
$size = New-Object System.Drawing.Size 720, 420
function Snap([string]$name) {
    $bmp = New-Object System.Drawing.Bitmap $size.Width, $size.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($pt.X - 360, $pt.Y - 200, 0, 0, $size)
    $bmp.Save((Join-Path $OutDir $name), [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Host ("saved {0}" -f $name)
}
Snap "0-before.png"

# 2) 探测器：产品激活（失败时探测器内用 AttachThreadInput 解锁重试）+ 锚点点击 + 信号复测
$probe = Join-Path $PSScriptRoot '..\..\target\debug\focus_probe.exe'
& $probe --hwnd $Hwnd --click-only --click ("{0},{1}" -f $X, $Y)

# 3) 点击后连拍三张（覆盖 blinking caret 的关/开相位）
Start-Sleep -Milliseconds 100
Snap "1-after-100ms.png"
Start-Sleep -Milliseconds 300
Snap "2-after-400ms.png"
Start-Sleep -Milliseconds 300
Snap "3-after-700ms.png"
Write-Host ("outdir={0}" -f $OutDir)
