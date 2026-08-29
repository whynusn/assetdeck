# D61 一键迁移真机验证驱动：分阶段控制沙箱里的 asset-manager.exe。
# 用法：
#   migrate-verify.ps1 launch -ExeDir <dir> -LibRoot <dir>   # 启动并等主窗口，打印 hwnd/rect
#   migrate-verify.ps1 snap -Hwnd <n> -Out <png>             # 整客户区截图
#   migrate-verify.ps1 crop -Out <png> -Crop L,T,W,H         # 对已有截图 2x 裁剪放大
#   migrate-verify.ps1 click -Hwnd <n> -X <clientX> -Y <clientY>
#   migrate-verify.ps1 close -Hwnd <n>
param(
    [Parameter(Mandatory = $true, Position = 0)][string]$Action,
    [long]$Hwnd = 0,
    [string]$ExeDir = '',
    [string]$LibRoot = '',
    [string]$Out = '',
    [string]$Crop = '',
    [int]$X = 0,
    [int]$Y = 0
)
$ErrorActionPreference = 'Stop'
Add-Type -Path (Join-Path $PSScriptRoot 'WEnum.cs')
Add-Type -AssemblyName System.Drawing
[WEnum]::SetProcessDPIAware() | Out-Null

function Find-WindowOfPid([int]$ProcId) {
    $script:hit = [IntPtr]::Zero
    $cb = [WEnum+EnumProc]{
        param([IntPtr]$h, [IntPtr]$lp)
        $owner = 0
        [WEnum]::GetWindowThreadProcessId($h, [ref]$owner) | Out-Null
        if ($owner -eq $ProcId -and [WEnum]::IsWindowVisible($h)) {
            $script:hit = $h
            return $false
        }
        return $true
    }
    [WEnum]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    $script:hit
}

switch ($Action) {
    'launch' {
        $p = Start-Process -FilePath (Join-Path $ExeDir 'asset-manager.exe') `
            -ArgumentList @('--library-root', $LibRoot) -WorkingDirectory $ExeDir -PassThru
        $hwnd = [IntPtr]::Zero
        for ($i = 0; $i -lt 100; $i++) {
            Start-Sleep -Milliseconds 200
            if ($p.HasExited) { Write-Host ("EXITED code=" + $p.ExitCode); exit 1 }
            $hwnd = Find-WindowOfPid $p.Id
            if ($hwnd -ne [IntPtr]::Zero) { break }
        }
        if ($hwnd -eq [IntPtr]::Zero) { Write-Host 'NOWINDOW'; exit 1 }
        $rect = New-Object WEnum+RECT
        [WEnum]::GetClientRect($hwnd, [ref]$rect) | Out-Null
        Write-Host ("OK pid={0} hwnd={1} client={2}x{3}" -f $p.Id, $hwnd, ($rect.R - $rect.L), ($rect.B - $rect.T))
    }
    'snap' {
        $h = [IntPtr]$Hwnd
        $rect = New-Object WEnum+RECT
        [WEnum]::GetClientRect($h, [ref]$rect) | Out-Null
        $pt = New-Object WEnum+POINT; $pt.X = 0; $pt.Y = 0
        [WEnum]::ClientToScreen($h, [ref]$pt) | Out-Null
        $w = $rect.R - $rect.L; $hh = $rect.B - $rect.T
        $bmp = New-Object System.Drawing.Bitmap $w, $hh
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($pt.X, $pt.Y, 0, 0, (New-Object System.Drawing.Size $w, $hh))
        $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
        $g.Dispose(); $bmp.Dispose()
        Write-Host ("OK snap {0} ({1}x{2})" -f $Out, $w, $hh)
    }
    'crop' {
        $full = [System.Drawing.Bitmap]::FromFile($Out)
        $p2 = $Crop.Split(',') | ForEach-Object { [int]$_ }
        $c = New-Object System.Drawing.Bitmap ($p2[2] * 2), ($p2[3] * 2)
        $g = [System.Drawing.Graphics]::FromImage($c)
        $g.InterpolationMode = 'NearestNeighbor'
        $g.DrawImage($full, (New-Object System.Drawing.Rectangle(0, 0, $p2[2] * 2, $p2[3] * 2)), (New-Object System.Drawing.Rectangle($p2[0], $p2[1], $p2[2], $p2[3])), 'Pixel')
        $c.Save(($Out -replace '\.png$', '-crop.png'), [System.Drawing.Imaging.ImageFormat]::Png)
        $g.Dispose(); $c.Dispose(); $full.Dispose()
        Write-Host 'OK crop'
    }
    'click' {
        $h = [IntPtr]$Hwnd
        $pt = New-Object WEnum+POINT; $pt.X = $X; $pt.Y = $Y
        [WEnum]::ClientToScreen($h, [ref]$pt) | Out-Null
        # SendInput 的 ABSOLUTE 移动在本机实测不生效（点击落在光标原位），
        # 改 SetCursorPos 定位 + mouse_event 注入按键（同会话同完整性，可靠）。
        [WEnum]::SetCursorPos($pt.X, $pt.Y) | Out-Null
        Start-Sleep -Milliseconds 80
        [WEnum]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero)  # LEFTDOWN
        Start-Sleep -Milliseconds 40
        [WEnum]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero)  # LEFTUP
        Start-Sleep -Milliseconds 120
        $cp = New-Object WEnum+POINT
        [WEnum]::GetCursorPos([ref]$cp) | Out-Null
        $fg = [WEnum]::GetForegroundWindow()
        Write-Host ("OK click client=({0},{1}) screen=({2},{3}) cursor=({4},{5}) fg={6}" -f $X, $Y, $pt.X, $pt.Y, $cp.X, $cp.Y, $fg)
    }
    'move' {
        $pt = New-Object WEnum+POINT; $pt.X = $X; $pt.Y = $Y
        $sx = [int]($pt.X * 65535 / [WEnum]::GetSystemMetrics(0))
        $sy = [int]($pt.Y * 65535 / [WEnum]::GetSystemMetrics(1))
        $size = [System.Runtime.InteropServices.Marshal]::SizeOf([type][WEnum+INPUT])
        $i = New-Object WEnum+INPUT
        $i.type = 0; $i.mi.dx = $sx; $i.mi.dy = $sy; $i.mi.dwFlags = 0x8001
        $arr = @($i)
        $sent = [WEnum]::SendInput(1, $arr, $size)
        Start-Sleep -Milliseconds 150
        $cp = New-Object WEnum+POINT
        [WEnum]::GetCursorPos([ref]$cp) | Out-Null
        Write-Host ("OK move screen=({0},{1}) norm=({2},{3}) sent={4} cursor=({5},{6}) sm={7}x{8}" -f $X, $Y, $sx, $sy, $sent, $cp.X, $cp.Y, [WEnum]::GetSystemMetrics(0), [WEnum]::GetSystemMetrics(1))
    }
    'close' {
        [WEnum]::PostMessage([IntPtr]$Hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null  # WM_CLOSE
        Write-Host 'OK close posted'
    }
    default { Write-Host ("UNKNOWN action " + $Action); exit 1 }
}
