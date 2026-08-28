# 测量 SendMessageTimeout(hwnd, WM_NULL) 作为「目标 UI 线程已处理完我们的激活」
# 的同步信号。WM_NULL 不改变任何状态，但这个调用会阻塞在内核里，直到目标窗口
# 所属线程把消息队列处理到这条 ping —— 也就是它已经消化完 SetForegroundWindow，
# 回到能接收输入的状态。它是**线程精确**（针对具体 hwnd 的线程，不是进程首线程）
# 且**事件驱动**（内核阻塞，非轮询、非固定睡眠）的就绪信号。
param(
  [Parameter(Mandatory=$true)][int]$Hwnd,
  [int]$Rounds = 6
)

$src = @'
using System;
using System.Runtime.InteropServices;
public static class P {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll", SetLastError=true)]
  public static extern IntPtr SendMessageTimeout(IntPtr h, uint msg, IntPtr wp, IntPtr lp, uint flags, uint ms, out IntPtr res);
}
'@
Add-Type -TypeDefinition $src -Language CSharp | Out-Null

$h = [IntPtr]$Hwnd
$WM_NULL = 0
$SMTO_ABORTIFHUNG = 0x0002
Write-Host "hwnd=$Hwnd"

for ($r = 1; $r -le $Rounds; $r++) {
  [P]::ShowWindow($h, 6) | Out-Null   # minimize
  Start-Sleep -Milliseconds 120
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  [P]::ShowWindow($h, 9) | Out-Null   # restore
  [P]::SetForegroundWindow($h) | Out-Null
  $tActivate = $sw.Elapsed.TotalMilliseconds
  [IntPtr]$res = [IntPtr]::Zero
  $ok = [P]::SendMessageTimeout($h, $WM_NULL, [IntPtr]::Zero, [IntPtr]::Zero, $SMTO_ABORTIFHUNG, 1500, [ref]$res)
  $tPing = $sw.Elapsed.TotalMilliseconds
  $fg = ([P]::GetForegroundWindow() -eq $h)
  Write-Host ("round {0}: activate={1:N1}ms  WM_NULL returned(ok={2}) at {3:N1}ms  ping_cost={4:N1}ms  fg==target={5}" -f `
    $r, $tActivate, ($ok -ne [IntPtr]::Zero), $tPing, ($tPing - $tActivate), $fg)
  Start-Sleep -Milliseconds 150
}
