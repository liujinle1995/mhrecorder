# DPI-aware window capture (user32 P/Invoke only).
param([string]$ProcessName="MhRecorder", [string]$Out="shot.png")
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$src = @"
using System;
using System.Runtime.InteropServices;
public class Win32Dpi {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
}
"@
if (-not ("Win32Dpi" -as [type])) { Add-Type -TypeDefinition $src }
[void][Win32Dpi]::SetProcessDPIAware()
$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { Write-Error "process not running"; exit 1 }
$hwnd = $proc.MainWindowHandle
$root = [System.Windows.Automation.AutomationElement]::RootElement
$cond = New-Object System.Windows.Automation.PropertyCondition(
  [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $proc.Id)
# The process may own small popups (tooltips, flyouts) that Win32 reports as
# MainWindowHandle; take the LARGEST top-level window instead of the first one,
# otherwise the shot ends up being a 237x39 tooltip.
$wins = $root.FindAll([System.Windows.Automation.TreeScope]::Children, $cond)
$win = $null
$best = 0.0
foreach ($c in $wins) {
  $b = $c.Current.BoundingRectangle
  $area = $b.Width * $b.Height
  if ($area -gt $best) { $best = $area; $win = $c }
}
if ($win) {
  $rect = $win.Current.BoundingRectangle
  $hwnd = [IntPtr]$win.Current.NativeWindowHandle
  $w=[int]$rect.Width; $h=[int]$rect.Height
} else {
  $r = New-Object Win32Dpi+RECT; [void][Win32Dpi]::GetWindowRect($hwnd,[ref]$r)
  $w=$r.Right-$r.Left; $h=$r.Bottom-$r.Top
}
Write-Host "capturing ${w}x${h}"
[void][Win32Dpi]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 800
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
[void][Win32Dpi]::PrintWindow($hwnd, $hdc, 2)
$g.ReleaseHdc($hdc); $g.Dispose()
$full = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path (Get-Location).Path $Out }
$bmp.Save($full, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
Write-Host "saved $full"
