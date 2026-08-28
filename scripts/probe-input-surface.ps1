# 探测：微信/千牛在「激活 -> 前台」之后，到底有没有可被 USER32 层观测到的
# 输入表面信号（GetGUIThreadInfo 的 hwndFocus/hwndCaret/rcCaret），以及 WaitForInputIdle
# 是否可用作「已就绪」的事件。目的：找出比固定 settle_ms 更早、更可靠的放行条件。
#
# 用法：pwsh -NoProfile -File scripts\probe-input-surface.ps1 -Hwnd 2163916
param(
  [Parameter(Mandatory=$true)][int]$Hwnd,
  [int]$Samples = 60,
  [int]$IntervalMs = 10
)

$src = @'
using System;
using System.Runtime.InteropServices;

public static class Probe {
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int left, top, right, bottom; }

  [StructLayout(LayoutKind.Sequential)]
  public struct GUITHREADINFO {
    public int cbSize;
    public int flags;
    public IntPtr hwndActive;
    public IntPtr hwndFocus;
    public IntPtr hwndCapture;
    public IntPtr hwndMenuOwner;
    public IntPtr hwndMoveSize;
    public IntPtr hwndCaret;
    public RECT rcCaret;
  }

  [DllImport("user32.dll")]
  public static extern bool GetGUIThreadInfo(uint idThread, ref GUITHREADINFO lpgui);
  [DllImport("user32.dll")]
  public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")]
  public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")]
  public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")]
  public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
  [DllImport("user32.dll")]
  public static extern uint WaitForInputIdle(IntPtr hProcess, uint ms);
  [DllImport("kernel32.dll")]
  public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);

  public static GUITHREADINFO Info(uint tid) {
    GUITHREADINFO g = new GUITHREADINFO();
    g.cbSize = Marshal.SizeOf(typeof(GUITHREADINFO));
    GetGUIThreadInfo(tid, ref g);
    return g;
  }
}
'@

Add-Type -TypeDefinition $src -Language CSharp | Out-Null

$h = [IntPtr]$Hwnd
[uint32]$procId = 0
$tid = [Probe]::GetWindowThreadProcessId($h, [ref]$procId)
Write-Host "target hwnd=$Hwnd tid=$tid pid=$procId"

# 激活前快照
$g0 = [Probe]::Info($tid)
Write-Host ("BEFORE activate: focus=0x{0:X} caret=0x{1:X} flags=0x{2:X} caretRect=({3},{4},{5},{6})" -f `
  $g0.hwndFocus.ToInt64(), $g0.hwndCaret.ToInt64(), $g0.flags, $g0.rcCaret.left, $g0.rcCaret.top, $g0.rcCaret.right, $g0.rcCaret.bottom)

# 激活
$sw = [System.Diagnostics.Stopwatch]::StartNew()
[Probe]::ShowWindow($h, 9) | Out-Null  # SW_RESTORE
[Probe]::SetForegroundWindow($h) | Out-Null

# WaitForInputIdle：需要 PROCESS_QUERY_INFORMATION(0x0400)|SYNCHRONIZE(0x00100000)
$ph = [Probe]::OpenProcess(0x00100400, $false, $procId)
$wfi = [Probe]::WaitForInputIdle($ph, 1000)
Write-Host ("WaitForInputIdle returned {0} at {1}ms (0=idle/ready, 0x102=WAIT_TIMEOUT, 0xFFFFFFFF=error)" -f $wfi, $sw.ElapsedMilliseconds)

# 轮询 GUITHREADINFO，找到 focus/caret 首次出现的时刻
$firstFocusMs = -1
$firstCaretMs = -1
$fgMs = -1
for ($i = 0; $i -lt $Samples; $i++) {
  $now = $sw.ElapsedMilliseconds
  if ($fgMs -lt 0 -and [Probe]::GetForegroundWindow() -eq $h) { $fgMs = $now }
  $g = [Probe]::Info($tid)
  if ($firstFocusMs -lt 0 -and $g.hwndFocus -ne [IntPtr]::Zero) { $firstFocusMs = $now }
  if ($firstCaretMs -lt 0 -and $g.hwndCaret -ne [IntPtr]::Zero) { $firstCaretMs = $now }
  Start-Sleep -Milliseconds $IntervalMs
}
$gf = [Probe]::Info($tid)
Write-Host ("AFTER {0}ms: focus=0x{1:X} caret=0x{2:X} flags=0x{3:X} caretRect=({4},{5},{6},{7})" -f `
  $sw.ElapsedMilliseconds, $gf.hwndFocus.ToInt64(), $gf.hwndCaret.ToInt64(), $gf.flags, $gf.rcCaret.left, $gf.rcCaret.top, $gf.rcCaret.right, $gf.rcCaret.bottom)
Write-Host ("first foreground==target at {0}ms; first hwndFocus!=0 at {1}ms; first hwndCaret!=0 at {2}ms" -f $fgMs, $firstFocusMs, $firstCaretMs)
Write-Host "flags bits: GUI_CARETBLINKING=0x1 GUI_INMENUMODE=0x4 GUI_INMOVESIZE=0x2 GUI_POPUPMENUMODE=0x10 GUI_SYSTEMMENUMODE=0x8"
