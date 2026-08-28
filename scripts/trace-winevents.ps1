# 广谱事件抓取：对目标进程装一个 EVENT_MIN..EVENT_MAX 的 SetWinEventHook，
# 激活目标窗口后记录接下来若干毫秒内到达的**所有** WinEvent（事件号 + 对象号 + 毫秒）。
#
# 关键：激活必须复刻产品路径（先敲 Alt 释放前台锁，再 SetForegroundWindow），
# 否则对千牛这类应用，裸 SetForegroundWindow 只会让任务栏闪红、窗口并未真正到前台，
# 测出来的“就绪信号”是假的。脚本结束会打印 foreground==target 供核对。
#
# 用法：pwsh -NoProfile -File scripts\trace-winevents.ps1 -Hwnd 721614 -CaptureMs 1000
param(
  [Parameter(Mandatory=$true)][int]$Hwnd,
  [int]$CaptureMs = 900
)

Add-Type -AssemblyName System.Windows.Forms | Out-Null

$src = @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;

public static class Trace {
  public delegate void WinEventProc(IntPtr hHook, uint evt, IntPtr hwnd, int idObj, int idChild, uint tid, uint time);

  [DllImport("user32.dll")]
  public static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr mod, WinEventProc cb, uint pid, uint tid, uint flags);
  [DllImport("user32.dll")]
  public static extern bool UnhookWinEvent(IntPtr h);
  [DllImport("user32.dll")]
  public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")]
  public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")]
  public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
  [DllImport("user32.dll")]
  public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")]
  public static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr extra);

  public static uint TargetPid;
  public static Stopwatch Clock = new Stopwatch();
  public static List<string> Log = new List<string>();
  public static WinEventProc Callback;   // keep alive
  public static IntPtr Hook;

  public static void OnEvent(IntPtr hHook, uint evt, IntPtr hwnd, int idObj, int idChild, uint tid, uint time) {
    uint pid;
    GetWindowThreadProcessId(hwnd, out pid);
    if (pid != TargetPid) return;
    Log.Add(String.Format("{0,5}ms evt=0x{1:X4} obj={2} child={3} hwnd=0x{4:X}", Clock.ElapsedMilliseconds, evt, idObj, idChild, hwnd.ToInt64()));
  }

  public static void Start(uint pid) {
    TargetPid = pid;
    Callback = new WinEventProc(OnEvent);
    Hook = SetWinEventHook(0x00000001, 0x7FFFFFFF, IntPtr.Zero, Callback, 0, 0, 0);
    Clock.Start();
  }

  // 复刻产品激活路径：先敲一次 Alt（按下+抬起）释放当前前台线程的输入所有权，
  // 再 SetForegroundWindow，才能绕过 Windows 前台锁真正把目标拉到前台。
  public static void Activate(IntPtr h) {
    ShowWindow(h, 9);           // SW_RESTORE
    keybd_event(0x12, 0, 0, IntPtr.Zero);        // VK_MENU down
    keybd_event(0x12, 0, 0x0002, IntPtr.Zero);   // VK_MENU up
    SetForegroundWindow(h);
  }

  public static void Stop() { UnhookWinEvent(Hook); }
}
'@

Add-Type -TypeDefinition $src -Language CSharp | Out-Null

$h = [IntPtr]$Hwnd
[uint32]$procId = 0
[Trace]::GetWindowThreadProcessId($h, [ref]$procId) | Out-Null
Write-Host "target hwnd=$Hwnd pid=$procId capture=${CaptureMs}ms"

[Trace]::Start($procId)
[Trace]::Activate($h)

$deadline = (Get-Date).AddMilliseconds($CaptureMs)
while ((Get-Date) -lt $deadline) {
  [System.Windows.Forms.Application]::DoEvents()
  Start-Sleep -Milliseconds 2
}
[Trace]::Stop()

$fg = [Trace]::GetForegroundWindow()
Write-Host ("foreground now = 0x{0:X} (target=0x{1:X})  ACTIVATED={2}" -f $fg.ToInt64(), $h.ToInt64(), ($fg -eq $h))
Write-Host "---- events from target pid ($($([Trace]::Log).Count) total) ----"
[Trace]::Log | ForEach-Object { Write-Host $_ }

Write-Host "---- event id legend (common) ----"
Write-Host "0x0003 SYSTEM_FOREGROUND  0x0016 SYSTEM_MINIMIZEEND  0x0017 SYSTEM_MINIMIZESTART"
Write-Host "0x8000 OBJECT_CREATE 0x8001 OBJECT_DESTROY 0x8002 OBJECT_SHOW 0x8003 OBJECT_HIDE"
Write-Host "0x8004 OBJECT_REORDER 0x8005 OBJECT_FOCUS 0x8006 OBJECT_SELECTION 0x800B OBJECT_LOCATIONCHANGE"
Write-Host "0x800C OBJECT_NAMECHANGE 0x800E OBJECT_VALUECHANGE 0x8017 OBJECT_LIVEREGIONCHANGED"
Write-Host "obj legend: -4=OBJID_CARET 0=OBJID_WINDOW -3=OBJID_CLIENT"
