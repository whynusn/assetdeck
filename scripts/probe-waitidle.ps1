# 精确测量 WaitForInputIdle 作为「输入表面就绪」事件的可用性：
# 反复「窗口切到后台 -> 激活 -> 立刻 WaitForInputIdle」，记录每次真正阻塞的毫秒数。
# 如果它稳定在几十毫秒内返回 0，就是一个真正的事件型放行信号（OS 在目标消息队列
# 变空闲时唤醒我们），可用来取代固定 settle_ms。
param(
  [Parameter(Mandatory=$true)][int]$Hwnd,
  [int]$Rounds = 6
)

$src = @'
using System;
using System.Runtime.InteropServices;
public static class W {
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern IntPtr GetDesktopWindow();
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("kernel32.dll")] public static extern IntPtr OpenProcess(uint a, bool i, uint pid);
  [DllImport("user32.dll")] public static extern uint WaitForInputIdle(IntPtr h, uint ms);
}
'@
Add-Type -TypeDefinition $src -Language CSharp | Out-Null

$h = [IntPtr]$Hwnd
[uint32]$procId = 0
[W]::GetWindowThreadProcessId($h, [ref]$procId) | Out-Null
$ph = [W]::OpenProcess(0x00100400, $false, $procId)   # QUERY_INFORMATION|SYNCHRONIZE
Write-Host "hwnd=$Hwnd pid=$procId"

for ($r = 1; $r -le $Rounds; $r++) {
  # 先把目标压到最小化，制造「刚被唤醒」的真实场景
  [W]::ShowWindow($h, 6) | Out-Null   # SW_MINIMIZE
  Start-Sleep -Milliseconds 120
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  [W]::ShowWindow($h, 9) | Out-Null   # SW_RESTORE
  [W]::SetForegroundWindow($h) | Out-Null
  $tActivate = $sw.Elapsed.TotalMilliseconds
  $wi = [W]::WaitForInputIdle($ph, 1500)
  $tIdle = $sw.Elapsed.TotalMilliseconds
  $fg = ([W]::GetForegroundWindow() -eq $h)
  Write-Host ("round {0}: activate call={1:N1}ms  WaitForInputIdle ret={2} blocked_until={3:N1}ms  fg==target={4}" -f `
    $r, $tActivate, $wi, $tIdle, $fg)
  Start-Sleep -Milliseconds 150
}
Write-Host "ret legend: 0=idle/ready  0x102=timeout  0xFFFFFFFF=error"
