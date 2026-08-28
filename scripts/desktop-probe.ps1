# 诊断脚本：DPI 感知的窗口截图 / 点击辅助。
# 仅用于真实 IM 上框验证，不属于产品路径，也不参与 cargo workspace。
param(
    [Parameter(Mandatory = $true)][ValidateSet('shot', 'screen', 'click', 'dblclick', 'clickabs', 'dblclickabs', 'rect')][string]$Action,
    [Parameter(Mandatory = $false)][long]$Hwnd = 0,
    [int]$X = 0,
    [int]$Y = 0,
    [string]$Out = ''
)

Add-Type -AssemblyName System.Drawing

$source = @'
using System;
using System.Runtime.InteropServices;

public static class DesktopProbe
{
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int dx, int dy, uint data, IntPtr extra);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr extra);

    public struct RECT { public int L, T, R, B; }

    public static void MakeDpiAware()
    {
        // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 = -4
        SetProcessDpiAwarenessContext(new IntPtr(-4));
    }

    public static void Activate(IntPtr h)
    {
        keybd_event(0x12, 0, 0, IntPtr.Zero);
        keybd_event(0x12, 0, 2, IntPtr.Zero);
        ShowWindow(h, 9);
        BringWindowToTop(h);
        SetForegroundWindow(h);
    }

    public static void Click(int x, int y)
    {
        SetCursorPos(x, y);
        System.Threading.Thread.Sleep(150);
        mouse_event(0x0002, 0, 0, 0, IntPtr.Zero);
        mouse_event(0x0004, 0, 0, 0, IntPtr.Zero);
    }

    public static void DoubleClick(int x, int y)
    {
        SetCursorPos(x, y);
        System.Threading.Thread.Sleep(150);
        mouse_event(0x0002, 0, 0, 0, IntPtr.Zero);
        mouse_event(0x0004, 0, 0, 0, IntPtr.Zero);
        System.Threading.Thread.Sleep(60);
        mouse_event(0x0002, 0, 0, 0, IntPtr.Zero);
        mouse_event(0x0004, 0, 0, 0, IntPtr.Zero);
    }
}
'@

if (-not ('DesktopProbe' -as [type])) {
    Add-Type -TypeDefinition $source
}

[DesktopProbe]::MakeDpiAware() | Out-Null
$handle = [IntPtr]$Hwnd
$rect = New-Object DesktopProbe+RECT

switch ($Action) {
    'rect' {
        [DesktopProbe]::GetWindowRect($handle, [ref]$rect) | Out-Null
        "rect={0},{1},{2},{3} size={4}x{5}" -f $rect.L, $rect.T, $rect.R, $rect.B, ($rect.R - $rect.L), ($rect.B - $rect.T)
    }
    'shot' {
        [DesktopProbe]::Activate($handle)
        Start-Sleep -Milliseconds 600
        [DesktopProbe]::GetWindowRect($handle, [ref]$rect) | Out-Null
        $width = $rect.R - $rect.L
        $height = $rect.B - $rect.T
        if ($width -le 0 -or $height -le 0) { throw "窗口尺寸无效: ${width}x${height}" }
        $bitmap = New-Object System.Drawing.Bitmap($width, $height)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $graphics.CopyFromScreen($rect.L, $rect.T, 0, 0, $bitmap.Size)
        if ([string]::IsNullOrWhiteSpace($Out)) { $Out = Join-Path $PWD ("probe-{0}.png" -f $Hwnd) }
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null
        $bitmap.Save($Out)
        $graphics.Dispose()
        $bitmap.Dispose()
        "saved={0} rect={1},{2} size={3}x{4}" -f $Out, $rect.L, $rect.T, $width, $height
    }
    'screen' {
        # 整屏截图：Slint 下拉浮层会让宿主窗口 rect 失真，只有整屏能看到真实弹层。
        Add-Type -AssemblyName System.Windows.Forms
        $bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
        $bitmap = New-Object System.Drawing.Bitmap($bounds.Width, $bounds.Height)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $graphics.CopyFromScreen($bounds.X, $bounds.Y, 0, 0, $bitmap.Size)
        if ([string]::IsNullOrWhiteSpace($Out)) { $Out = Join-Path $PWD 'probe-screen.png' }
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null
        $bitmap.Save($Out)
        $graphics.Dispose()
        $bitmap.Dispose()
        "saved={0} screen={1},{2} size={3}x{4}" -f $Out, $bounds.X, $bounds.Y, $bounds.Width, $bounds.Height
    }
    'click' {
        [DesktopProbe]::Activate($handle)
        Start-Sleep -Milliseconds 600
        [DesktopProbe]::GetWindowRect($handle, [ref]$rect) | Out-Null
        $absX = $rect.L + $X
        $absY = $rect.T + $Y
        [DesktopProbe]::Click($absX, $absY)
        Start-Sleep -Milliseconds 800
        "clicked={0},{1} foreground={2}" -f $absX, $absY, [long][DesktopProbe]::GetForegroundWindow()
    }
    'dblclick' {
        [DesktopProbe]::Activate($handle)
        Start-Sleep -Milliseconds 600
        [DesktopProbe]::GetWindowRect($handle, [ref]$rect) | Out-Null
        $absX = $rect.L + $X
        $absY = $rect.T + $Y
        [DesktopProbe]::DoubleClick($absX, $absY)
        Start-Sleep -Milliseconds 1200
        "dblclicked={0},{1} foreground={2}" -f $absX, $absY, [long][DesktopProbe]::GetForegroundWindow()
    }
    'clickabs' {
        # 绝对屏幕坐标点击：Slint 浮层期 GetWindowRect 失真，不能用窗口相对偏移。
        if ($handle -ne [IntPtr]::Zero) { [DesktopProbe]::Activate($handle); Start-Sleep -Milliseconds 600 }
        [DesktopProbe]::Click($X, $Y)
        Start-Sleep -Milliseconds 800
        "clicked={0},{1} foreground={2}" -f $X, $Y, [long][DesktopProbe]::GetForegroundWindow()
    }
    'dblclickabs' {
        if ($handle -ne [IntPtr]::Zero) { [DesktopProbe]::Activate($handle); Start-Sleep -Milliseconds 600 }
        [DesktopProbe]::DoubleClick($X, $Y)
        Start-Sleep -Milliseconds 1200
        "dblclicked={0},{1} foreground={2}" -f $X, $Y, [long][DesktopProbe]::GetForegroundWindow()
    }
}
