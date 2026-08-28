# 诊断脚本：枚举目标 IM 进程的全部顶层窗口，输出 class/title/可见性/尺寸/样式。
# 用于回填 profiles.builtin.toml 的画像淘汰规则，不属于产品路径。
param([string[]]$Process = @('Weixin', 'AliWorkbench', 'PddWorkbench', 'Telegram'))

$source = @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public static class ImWindowEnum
{
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern int GetWindowLongW(IntPtr h, int index);
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);

    public struct RECT { public int L, T, R, B; }
    delegate bool EnumProc(IntPtr h, IntPtr l);

    public static List<string> Run()
    {
        SetProcessDpiAwarenessContext(new IntPtr(-4));
        List<string> rows = new List<string>();
        EnumWindows(delegate(IntPtr h, IntPtr l)
        {
            uint pid;
            GetWindowThreadProcessId(h, out pid);
            StringBuilder cls = new StringBuilder(256);
            GetClassNameW(h, cls, 256);
            StringBuilder title = new StringBuilder(512);
            GetWindowTextW(h, title, 512);
            RECT r;
            GetWindowRect(h, out r);
            rows.Add(string.Join("\t", new string[] {
                pid.ToString(),
                ((long)h).ToString(),
                cls.ToString(),
                title.ToString(),
                IsWindowVisible(h) ? "1" : "0",
                IsIconic(h) ? "1" : "0",
                (r.R - r.L).ToString(),
                (r.B - r.T).ToString(),
                "0x" + GetWindowLongW(h, -16).ToString("X8"),
                "0x" + GetWindowLongW(h, -20).ToString("X8")
            }));
            return true;
        }, IntPtr.Zero);
        return rows;
    }
}
'@

if (-not ('ImWindowEnum' -as [type])) {
    Add-Type -TypeDefinition $source
}

$byPid = @{}
foreach ($name in $Process) {
    foreach ($proc in Get-Process -Name $name -ErrorAction SilentlyContinue) {
        $byPid[[uint32]$proc.Id] = $proc.ProcessName
    }
}

foreach ($row in [ImWindowEnum]::Run()) {
    $parts = $row -split "`t"
    $procId = [uint32]$parts[0]
    if (-not $byPid.ContainsKey($procId)) { continue }
    [pscustomobject]@{
        Exe      = $byPid[$procId]
        Pid      = $procId
        Hwnd     = [long]$parts[1]
        Class    = $parts[2]
        Title    = $parts[3]
        Visible  = $parts[4]
        Min      = $parts[5]
        Size     = "$($parts[6])x$($parts[7])"
        Style    = $parts[8]
        ExStyle  = $parts[9]
    }
}
