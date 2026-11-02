# requires: powershell. Runs an exe, finds its top-level window by PID, screenshots ONLY that window.
param(
    [Parameter(Mandatory=$true)][string]$Exe,
    [Parameter(Mandatory=$true)][string]$Out,
    [int]$WaitSec = 12,
    [string]$ClickAutomationId = "",
    [int]$Width = 0,
    [int]$Height = 0,
    [switch]$ScrollToBottom,
    [switch]$KeepAlive
)

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms

$sig = @'
using System;
using System.Runtime.InteropServices;
public class W {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, ref RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, ref RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("dwmapi.dll")] public static extern bool DwmGetWindowAttribute(IntPtr h, int attr, ref RECT r, int size);
}
public static class DpiSetup {
    public static void MakePerMonitorV2() {
        // -4 == DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
        try { W.SetProcessDpiAwarenessContext(new IntPtr(-4)); } catch { }
    }
}
public static class Dwm {
    public static bool GetWindowFrameBounds(IntPtr h, ref W.RECT r) {
        // DWMWA_EXTENDED_FRAME_BOUNDS == 9
        return W.DwmGetWindowAttribute(h, 9, ref r, System.Runtime.InteropServices.Marshal.SizeOf(typeof(W.RECT)));
    }
}
'@
Add-Type -TypeDefinition $sig -ErrorAction Stop
[DpiSetup]::MakePerMonitorV2()

$SWP_NOSIZE = 0x0001
$SWP_NOMOVE = 0x0002
$SWP_SHOWWINDOW = 0x0040
$HWND_TOP = [IntPtr]0

$p = Start-Process -FilePath $Exe -PassThru
Start-Sleep -Seconds $WaitSec

if ($p.HasExited) {
    Write-Output "PROCESS_EXITED_EARLY code=$($p.ExitCode)"
    exit 1
}

$root = [System.Windows.Automation.AutomationElement]::RootElement
$pidCond = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $p.Id)
$win = $null
for ($i = 0; $i -lt 40; $i++) {
    $win = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $pidCond)
    if ($win -ne $null) { break }
    Start-Sleep -Milliseconds 500
}
if ($win -eq $null) {
    Write-Output "NO_WINDOW_FOUND pid=$($p.Id)"
    exit 1
}

$hwnd = [IntPtr]$win.Current.NativeWindowHandle

if ([W]::IsZoomed($hwnd)) {
    [void][W]::ShowWindow($hwnd, 9)   # SW_RESTORE undoes the maximize animation
    Start-Sleep -Milliseconds 1200
}

if ($ClickAutomationId -ne "") {
    # Comma-separated list: "NavSettings,ViewChangelogButton" clicks in order,
    # which is how dialogs behind a nav item get captured. A step prefixed with
    # "name=" matches the element Name (used for dialog buttons, which have no
    # AutomationId of their own).
    foreach ($id in ($ClickAutomationId -split '\s*,\s*')) {
        if ($id -eq "") { continue }
        $cond = if ($id.StartsWith("name=")) {
            New-Object System.Windows.Automation.PropertyCondition(
                [System.Windows.Automation.AutomationElement]::NameProperty, $id.Substring(5))
        } else {
            New-Object System.Windows.Automation.PropertyCondition(
                [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $id)
        }
        $btn = $win.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
        if ($btn -ne $null) {
            # List items expose SelectionItem, buttons expose Invoke; try both so
            # a window-picker row can be selected by its title.
            $invoked = $false
            try {
                $btn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
                $invoked = $true
            } catch { }
            if (-not $invoked) {
                try {
                    $btn.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
                    $invoked = $true
                } catch { }
            }
            if (-not $invoked) {
                Write-Output "NO_INVOKE_OR_SELECT $id"
            }
            Start-Sleep -Milliseconds 1500
        } else {
            Write-Output "AUTOMATION_ID_NOT_FOUND $id"
        }
    }
}

if ($ScrollToBottom) {
    # Scroll the first scrollable element to its end, so long pages can be
    # captured bottom-first (settings' 关于 section sits below the fold).
    $scrollCond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::IsScrollPatternAvailableProperty, $true)
    $scroller = $win.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $scrollCond)
    if ($scroller -ne $null) {
        $scroller.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern).SetScrollPercent(-1, 100)
        Start-Sleep -Milliseconds 800
    } else {
        Write-Output "NO_SCROLLABLE_ELEMENT"
    }
}

if ($Width -gt 0 -and $Height -gt 0) {
    $wa = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    $x = $wa.Left + [Math]::Max(0, ($wa.Width - $Width) / 2)
    $y = $wa.Top  + [Math]::Max(0, ($wa.Height - $Height) / 2)
    [void][W]::SetWindowPos($hwnd, $HWND_TOP, [int]$x, [int]$y, $Width, $Height, $SWP_SHOWWINDOW)
} else {
    [void][W]::SetWindowPos($hwnd, $HWND_TOP, 0, 0, 0, 0, $SWP_NOSIZE -bor $SWP_NOMOVE -bor $SWP_SHOWWINDOW)
}
[void][W]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 2500

$r = New-Object W+RECT
[void][W]::GetWindowRect($hwnd, [ref]$r)

# Capture the CLIENT area only. GetWindowRect on a WinUI window includes an
# invisible resize grip, so capturing from it lets desktop pixels bleed in.
$cr = New-Object W+RECT
[void][W]::GetClientRect($hwnd, [ref]$cr)
$origin = New-Object W+POINT
[void][W]::ClientToScreen($hwnd, [ref]$origin)
$r.Left = $origin.X
$r.Top = $origin.Y
$r.Right = $origin.X + $cr.Right
$r.Bottom = $origin.Y + $cr.Bottom

$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
if ($w -le 0 -or $h -le 0) { Write-Output "BAD_RECT $w x $h"; exit 1 }

$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen((New-Object System.Drawing.Point $r.Left, $r.Top),
                  [System.Drawing.Point]::Empty,
                  (New-Object System.Drawing.Size $w, $h))
$dir = Split-Path -Parent $Out
if (!(Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()

Write-Output "OK pid=$($p.Id) hwnd=$hwnd rect=$($r.Left),$($r.Top) size=${w}x${h} -> $Out"

if (-not $KeepAlive) {
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
}
exit 0
