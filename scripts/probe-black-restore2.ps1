# probe-black-restore2.ps1 - [DEBUG repro loop v3] variant stress: maximize / resize-hidden / showdesktop / fast / dwell
param(
  [int]$Cycles = 40,
  [string]$Exe = "$PSScriptRoot\..\target\release\asset-manager.exe",
  [string]$OutDir = "$PSScriptRoot\..\artifacts\debug\black-restore",
  [int]$Seed = 42,
  [string]$Variant = 'plain',     # plain | max | resizeHidden | showDesktop | fast | dwell
  [int]$SampleDelayMs = -1        # -1 = variant default
)
$ErrorActionPreference = 'Continue'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class W3 {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hgt, uint flags);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public struct RECT { public int L, T, R, B; }
  public static List<IntPtr> VisibleWindowsOf(uint pid) {
    var list = new List<IntPtr>();
    EnumWindows((h, l) => {
      uint p; GetWindowThreadProcessId(h, out p);
      if (p == pid && IsWindowVisible(h)) {
        RECT r; GetWindowRect(h, out r);
        if (r.R - r.L > 200 && r.B - r.T > 200) list.Add(h);
      }
      return true;
    }, IntPtr.Zero);
    return list;
  }
}
"@
[W3]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$csv = Join-Path $OutDir ("cycles-{0}.csv" -f $Variant)
'cycle,ratio,verdict' | Set-Content $csv
$tag = "{0}-s{1}" -f $Variant, $Seed

function Get-Ratio([System.Drawing.Bitmap]$b, [int]$x0, [int]$y0, [int]$x1, [int]$y1) {
  $tot = 0; $blk = 0
  for ($y = $y0; $y -lt $y1; $y += 4) {
    for ($x = $x0; $x -lt $x1; $x += 4) {
      $p = $b.GetPixel($x, $y); $tot++
      if ($p.R -eq 0 -and $p.G -eq 0 -and $p.B -eq 0) { $blk++ }
    }
  }
  if ($tot -eq 0) { return 1.0 }
  return $blk / $tot
}

function Capture-Ratio([IntPtr]$hwnd, [string]$tagFile) {
  $r = New-Object W3+RECT
  [W3]::GetWindowRect($hwnd, [ref]$r) | Out-Null
  $w = $r.R - $r.L; $h = $r.B - $r.T
  if ($w -le 0 -or $h -le 0) { return @{ ratio = 1.0 } }
  $bmp = New-Object System.Drawing.Bitmap($w, $h)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($r.L, $r.T, 0, 0, (New-Object System.Drawing.Size($w, $h)))
  $g.Dispose()
  $ratio = Get-Ratio $bmp 0 0 $w $h
  if ($ratio -gt 0.05) { $bmp.Save((Join-Path $OutDir ("red-{0}.png" -f $tagFile)) ) }
  $bmp.Dispose()
  return @{ ratio = $ratio }
}

$proc = Start-Process -FilePath $Exe -PassThru
$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 150; $i++) {
  Start-Sleep -Milliseconds 200
  if ($proc.HasExited) { break }
  $found = [W3]::VisibleWindowsOf([uint32]$proc.Id)
  if ($found.Count -gt 0) { $hwnd = $found[0]; break }
}
if ($hwnd -eq [IntPtr]::Zero) { Write-Host "FAIL: no visible window exited=$($proc.HasExited)"; exit 1 }
Start-Sleep -Seconds 2
[W3]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 300
$baseRes = Capture-Ratio $hwnd "baseline-$tag"
Write-Host ("[{0}] baseline ratio={1:P2}" -f $tag, $baseRes.ratio)

if ($Variant -eq 'max') { [W3]::ShowWindow($hwnd, 3) | Out-Null; Start-Sleep -Milliseconds 800 }  # SW_MAXIMIZE first

$rand = New-Object System.Random($Seed)
$red = 0
for ($c = 1; $c -le $Cycles; $c++) {
  if ($proc.HasExited) { Write-Host "APP DIED at cycle $c"; break }
  switch ($Variant) {
    'fast' {
      [W3]::ShowWindow($hwnd, 6) | Out-Null
      Start-Sleep -Milliseconds (30 + $rand.Next(60))
      [W3]::ShowWindow($hwnd, 9) | Out-Null
      Start-Sleep -Milliseconds (40 + $rand.Next(80))
      $delay = 30
    }
    'dwell' {
      [W3]::ShowWindow($hwnd, 6) | Out-Null
      Start-Sleep -Milliseconds (2500 + $rand.Next(1500))
      [W3]::ShowWindow($hwnd, 9) | Out-Null
      Start-Sleep -Milliseconds (800 + $rand.Next(400))
      $delay = 100
    }
    'resizeHidden' {
      [W3]::ShowWindow($hwnd, 6) | Out-Null
      Start-Sleep -Milliseconds 400
      $rw = 900 + ($c % 5) * 150
      $rh = 700 + ($c % 4) * 120
      [W3]::SetWindowPos($hwnd, [IntPtr]::Zero, 40 + ($c % 3) * 60, 40, $rw, $rh, 0x0004) | Out-Null  # SWP_NOZORDER
      Start-Sleep -Milliseconds 250
      [W3]::ShowWindow($hwnd, 9) | Out-Null
      Start-Sleep -Milliseconds 500
      $delay = 60
    }
    'showDesktop' {
      $sh = New-Object -ComObject Shell.Application
      $sh.ToggleDesktop() | Out-Null
      Start-Sleep -Milliseconds (700 + $rand.Next(400))
      [W3]::ShowWindow($hwnd, 9) | Out-Null
      Start-Sleep -Milliseconds (500 + $rand.Next(300))
      $delay = 80
    }
    default {
      [W3]::ShowWindow($hwnd, 6) | Out-Null
      Start-Sleep -Milliseconds (250 + $rand.Next(300))
      [W3]::ShowWindow($hwnd, 9) | Out-Null
      Start-Sleep -Milliseconds (350 + $rand.Next(350))
      $delay = 80
    }
  }
  Start-Sleep -Milliseconds $delay
  $res = Capture-Ratio $hwnd ("{0}-c{1:d3}" -f $tag, $c)
  $verdict = if ($res.ratio -gt 0.05) { 'RED' } else { 'green' }
  if ($verdict -eq 'RED') { $red++ }
  "{0},{1},{2}" -f $c, [math]::Round($res.ratio, 4), $verdict | Add-Content $csv
  if ($verdict -eq 'RED') { Write-Host ("[{0}] cycle {1} ratio={2:P2} -> RED" -f $tag, $c, $res.ratio) }
}
if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
Write-Host ("[{0}] DONE red={1}/{2}" -f $tag, $red, $Cycles)
