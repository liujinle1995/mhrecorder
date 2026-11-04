#requires -Version 5.1
<#
    角色管理页 Cover Flow 的运行时验证脚本。

    流程：用指定的数据目录启动界面 → 点进「角色管理」→ 按模式操作并逐个截图，
    最后把 ui.log 里的 carousel: 日志打出来（状态机轨迹 + 卡片创建次数）。

    用法：
        .\scripts\test_coverflow.ps1 -Tag 6char -DataDir D:\app\mhrecorder\build\cfdata-6 -Mode sequence
        .\scripts\test_coverflow.ps1 -Tag 2char -DataDir D:\app\mhrecorder\build\cfdata-2 -Mode static
#>
param(
    [Parameter(Mandatory = $true)][string]$Tag,
    [Parameter(Mandatory = $true)][string]$DataDir,
    [string]$Exe = 'D:\app\mhrecorder\dist\MhRecorder\MhRecorder.exe',
    [string]$OutDir = 'D:\app\mhrecorder\build\coverflow',
    [ValidateSet('static', 'sequence', 'delete', 'search', 'roundtrip')][string]$Mode = 'sequence'
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms

$sig = @'
using System;
using System.Runtime.InteropServices;
public class W {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, ref RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, ref RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int dx, int dy, int data, IntPtr extra);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);
}
public static class DpiSetup {
    public static void MakePerMonitorV2() {
        try { W.SetProcessDpiAwarenessContext(new IntPtr(-4)); } catch { }
    }
}
'@
Add-Type -TypeDefinition $sig -ErrorAction Stop
[DpiSetup]::MakePerMonitorV2()

if (!(Test-Path $OutDir)) { New-Item -ItemType Directory -Force -Path $OutDir | Out-Null }

$logPath = Join-Path $env:LOCALAPPDATA 'MhRecorder\ui.log'
$logLinesBefore = 0
if (Test-Path $logPath) { $logLinesBefore = (Get-Content $logPath -Encoding utf8).Count }

# ---- 清场 ----------------------------------------------------------------
Get-Process MhRecorder, mhservice -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 700

# ---- 启动（数据目录用环境变量指定，隔离真实数据） --------------------------
$prevDataDir = $env:MH_DATA_DIR
$env:MH_DATA_DIR = $DataDir
$app = Start-Process -FilePath $Exe -PassThru
if ($null -eq $prevDataDir) { Remove-Item Env:\MH_DATA_DIR -ErrorAction SilentlyContinue }
else { $env:MH_DATA_DIR = $prevDataDir }

$root = [System.Windows.Automation.AutomationElement]::RootElement
$pidCond = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $app.Id)
$win = $null
for ($i = 0; $i -lt 60; $i++) {
    Start-Sleep -Milliseconds 400
    $win = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $pidCond)
    if ($null -ne $win) { break }
}
if ($null -eq $win) { Write-Host "NO_WINDOW" -ForegroundColor Red; exit 1 }

$hwnd = [IntPtr]$win.Current.NativeWindowHandle
if ([W]::IsZoomed($hwnd)) { [void][W]::ShowWindow($hwnd, 9); Start-Sleep -Milliseconds 900 }
[void][W]::SetWindowPos($hwnd, [IntPtr]0, 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0040)
[void][W]::SetForegroundWindow($hwnd)

function Save-Shot([string]$name) {
    $cr = New-Object W+RECT
    [void][W]::GetClientRect($hwnd, [ref]$cr)
    $o = New-Object W+POINT
    [void][W]::ClientToScreen($hwnd, [ref]$o)
    $w = $cr.Right; $h = $cr.Bottom
    if ($w -le 0 -or $h -le 0) { Write-Host "  BAD_RECT ${w}x${h}"; return }
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen((New-Object System.Drawing.Point $o.X, $o.Y),
                      [System.Drawing.Point]::Empty,
                      (New-Object System.Drawing.Size $w, $h))
    $path = Join-Path $OutDir "$Tag-$name.png"
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Host ("  shot {0} ({1}x{2})" -f $name, $w, $h) -ForegroundColor DarkGray
}

function Find-Id([string]$id) {
    $c = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $id)
    return $win.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c)
}

function Invoke-Id([string]$id) {
    $el = Find-Id $id
    if ($null -eq $el) { Write-Host "  MISSING $id" -ForegroundColor Red; return $false }
    $el.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    return $true
}

function Invoke-Name([string]$name) {
    $c = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty, $name)
    $el = $win.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c)
    if ($null -eq $el) { Write-Host "  MISSING name=$name" -ForegroundColor Red; return $false }
    try { $el.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
    catch { $el.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select() }
    return $true
}

function Send-Wheel([int]$notches) {
    $cr = New-Object W+RECT
    [void][W]::GetClientRect($hwnd, [ref]$cr)
    $o = New-Object W+POINT
    [void][W]::ClientToScreen($hwnd, [ref]$o)
    [void][W]::SetCursorPos(($o.X + [int]($cr.Right / 2)), ($o.Y + [int]($cr.Bottom / 2)))
    for ($i = 0; $i -lt [Math]::Abs($notches); $i++) {
        [W]::mouse_event(0x0800, 0, 0, $(if ($notches -gt 0) { -120 } else { 120 }), [IntPtr]::Zero)
        Start-Sleep -Milliseconds 70
    }
}

# ---- 进入角色管理 --------------------------------------------------------
Start-Sleep -Seconds 3
if (-not (Invoke-Id 'NavCharacters')) { Write-Host "NO_NAV_CHARACTERS" -ForegroundColor Red }
Start-Sleep -Seconds 2
[void][W]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 600
Save-Shot '0-idle'

if ($Mode -eq 'static') {
    Save-Shot '1-static'
}
elseif ($Mode -eq 'sequence') {
    Write-Host "== 单步正向 ==" -ForegroundColor Cyan
    Invoke-Id 'CarouselNext' | Out-Null
    Start-Sleep -Milliseconds 130
    Save-Shot '1-next-mid'
    Start-Sleep -Milliseconds 600
    Save-Shot '2-next-settled'

    Write-Host "== 单步反向 ==" -ForegroundColor Cyan
    Invoke-Id 'CarouselPrev' | Out-Null
    Start-Sleep -Milliseconds 130
    Save-Shot '3-prev-mid'
    Start-Sleep -Milliseconds 600
    Save-Shot '4-prev-settled'

    Write-Host "== 连续 10 次点击 ==" -ForegroundColor Cyan
    for ($i = 0; $i -lt 10; $i++) {
        Invoke-Id 'CarouselNext' | Out-Null
        Start-Sleep -Milliseconds 70
    }
    Save-Shot '5-rapid-during'
    Start-Sleep -Milliseconds 1600
    Save-Shot '6-rapid-mid'
    Start-Sleep -Seconds 6
    Save-Shot '7-rapid-settled'

    Write-Host "== 滚轮 ==" -ForegroundColor Cyan
    Send-Wheel 6
    Start-Sleep -Milliseconds 900
    Save-Shot '8-wheel-mid'
    Start-Sleep -Seconds 5
    Save-Shot '9-wheel-settled'

    Write-Host "== 窗口缩放 ==" -ForegroundColor Cyan
    [void][W]::SetWindowPos($hwnd, [IntPtr]0, 60, 40, 1020, 720, 0x0040)
    Start-Sleep -Milliseconds 1200
    Save-Shot 'a-small'
    Invoke-Id 'CarouselNext' | Out-Null
    Start-Sleep -Milliseconds 900
    Save-Shot 'b-small-after-next'
    [void][W]::SetWindowPos($hwnd, [IntPtr]0, 0, 0, 1328, 900, 0x0040)
    Start-Sleep -Milliseconds 1200
    Save-Shot 'c-restored'
}
elseif ($Mode -eq 'roundtrip') {
    Write-Host "== 页面反复进出 ==" -ForegroundColor Cyan
    for ($round = 1; $round -le 3; $round++) {
        Invoke-Id 'CarouselNext' | Out-Null
        Start-Sleep -Milliseconds 700
        Invoke-Id 'NavAssets' | Out-Null
        Start-Sleep -Milliseconds 1300
        Invoke-Id 'NavSettings' | Out-Null
        Start-Sleep -Milliseconds 1300
        Invoke-Id 'NavCharacters' | Out-Null
        Start-Sleep -Milliseconds 1600
        Save-Shot ("round{0}" -f $round)
    }
    # 回来之后仍然要能正常翻页
    Invoke-Id 'CarouselNext' | Out-Null
    Start-Sleep -Milliseconds 1300
    Save-Shot 'after-roundtrips-next'
}
elseif ($Mode -eq 'search') {
    Write-Host "== 搜索过滤 ==" -ForegroundColor Cyan
    $box = Find-Id 'CharacterSearchBox'
    if ($null -eq $box) { Write-Host "  NO_SEARCH_BOX" -ForegroundColor Red }
    else {
        $vp = $box.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
        $vp.SetValue('莫')
        Start-Sleep -Milliseconds 1500
        Save-Shot '1-filter-one'
        Invoke-Id 'CarouselNext' | Out-Null
        Start-Sleep -Milliseconds 1000
        Save-Shot '2-filter-one-next'
        $vp.SetValue('测试')
        Start-Sleep -Milliseconds 1500
        Save-Shot '3-filter-three'
        Invoke-Id 'CarouselNext' | Out-Null
        Start-Sleep -Milliseconds 900
        Save-Shot '4-filter-three-next'
        $vp.SetValue('')
        Start-Sleep -Milliseconds 1500
        Save-Shot '5-cleared'
    }
}
elseif ($Mode -eq 'delete') {
    Write-Host "== 删除中心角色 ==" -ForegroundColor Cyan
    $winRect = New-Object W+RECT
    [void][W]::GetClientRect($hwnd, [ref]$winRect)
    $centerX = $winRect.Right / 2.0

    # 所有卡片的删除按钮里，挑横向最靠近窗口中心的那一个（也就是中心卡片）。
    $all = [System.Windows.Automation.Condition]::TrueCondition
    $els = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $all)
    $best = $null
    $bestDist = 1e9
    foreach ($el in $els) {
        $id = $el.Current.AutomationId
        if ($id -like 'Delete_*') {
            $r = $el.Current.BoundingRectangle
            if ($r.Width -le 0) { continue }
            $d = [Math]::Abs(($r.X + $r.Width / 2) - $centerX)
            if ($d -lt $bestDist) { $bestDist = $d; $best = $el }
        }
    }
    if ($null -eq $best) { Write-Host "NO_DELETE_BUTTON" -ForegroundColor Red }
    else {
        Write-Host ("  选中 {0}（距中心 {1:N0}px）" -f $best.Current.AutomationId, $bestDist)
        $best.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 900
        Save-Shot '1-confirm'
        Invoke-Name '删除' | Out-Null
        Start-Sleep -Milliseconds 500
        Save-Shot '2-deleting'
        Start-Sleep -Seconds 3
        Save-Shot '3-after-delete'
    }
}

# ---- 收尾 ----------------------------------------------------------------
Start-Sleep -Milliseconds 500
$app.Refresh()
Write-Host ""
Write-Host "--- ui.log (carousel) ---" -ForegroundColor Cyan
if (Test-Path $logPath) {
    $lines = Get-Content $logPath -Encoding utf8
    $new = $lines | Select-Object -Skip $logLinesBefore
    $carousel = $new | Where-Object { $_ -match 'carousel:' }
    $carousel | ForEach-Object { Write-Host $_ }
    $err = $new | Where-Object { $_ -match 'failed|Exception|未到达' }
    if ($err) { Write-Host "--- 异常/兜底 ---" -ForegroundColor Yellow; $err | ForEach-Object { Write-Host $_ } }
    Write-Host ("steps logged: {0}" -f ($carousel | Where-Object { $_ -match 'step ' }).Count)
    Write-Host ("cards built:  {0}" -f ($carousel | Where-Object { $_ -match 'card built' }).Count)
}

Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
Get-Process mhservice -ErrorAction SilentlyContinue | Stop-Process -Force
Write-Host "DONE $Tag" -ForegroundColor Green
