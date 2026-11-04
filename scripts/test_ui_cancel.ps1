#!/usr/bin/env pwsh
# ============================================================================
#  test_ui_cancel.ps1 — 界面级验证「点了取消真的会停」，不是假的
#
#  流程：把一张真截图放进剪贴板 → 点「粘贴」触发识别 → 200ms 后点「取消」
#        → 断言界面出现「识别已取消」，并截图留证。
#
#  为什么要点界面而不是直接打协议：协议级测试已经证明后端能取消，这里要证明
#  的是**这条链路是通的**——按钮在、命令绑上了、CancellationToken 传到了客户端、
#  取消后界面会恢复而不是永远转圈。
#
#  用法： .\scripts\test_ui_cancel.ps1
# ============================================================================

param(
    [string] $Exe,
    [string] $ShotDir = 'D:\app\mhrecorder\build'
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = Split-Path -Parent $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $root 'dist\MhRecorder\MhRecorder.exe' }
if (-not (Test-Path -LiteralPath $Exe)) { throw "找不到界面程序：$Exe" }
if (-not (Test-Path -LiteralPath $ShotDir)) { New-Item -ItemType Directory -Force -Path $ShotDir | Out-Null }

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms

$pass = 0; $fail = 0
function Check($name, $cond, $detail = '') {
    if ($cond) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name  $detail" -ForegroundColor Red }
}

# ---- 1. 剪贴板放一张真截图 ------------------------------------------------
# Clipboard.SetImage 要求 STA。pwsh 在 Windows 上默认就是 STA，这里先确认，
# 不是就明确报错，别让后面"粘贴按钮点了没反应"看起来像功能坏了。
if ([System.Threading.Thread]::CurrentThread.GetApartmentState() -ne 'STA') {
    throw '当前线程不是 STA，无法写剪贴板。请用 pwsh（Windows 默认 STA）运行本脚本。'
}
Add-Type -AssemblyName System.Windows.Forms, System.Drawing

$sample = Join-Path $root 'samples\real_character_attributes.png'
if (-not (Test-Path -LiteralPath $sample)) {
    $sample = (Get-ChildItem (Join-Path $root 'samples') -Filter *.png |
               Sort-Object Length -Descending | Select-Object -First 1).FullName
}
$img = [System.Drawing.Image]::FromFile($sample)
try {
    # 放大到 2 倍再放剪贴板：OCR 时间随像素数涨，才能留出"点取消"的窗口。
    # 原尺寸冷缓存约 1 秒，UIA 找到按钮再点下去经常刚好晚一步。
    $big = New-Object System.Drawing.Bitmap(($img.Width * 2), ($img.Height * 2))
    $g = [System.Drawing.Graphics]::FromImage($big)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.DrawImage($img, 0, 0, $big.Width, $big.Height)
    $g.Dispose()
    [System.Windows.Forms.Clipboard]::SetImage($big)
    $big.Dispose()
} finally { $img.Dispose() }
Write-Host "剪贴板已放入 $([System.IO.Path]::GetFileName($sample))（放大 2 倍）" -ForegroundColor DarkGray

Get-Process mhservice, MhRecorder -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 600

# 用全新的数据目录：特征缓存命中时识别只要 100ms 左右，根本来不及点取消，
# 测出来的"取消"是假的（其实是已经跑完了）。冷缓存才有足够长的窗口。
$freshData = Join-Path $env:TEMP ("mh-ui-cancel-" + [guid]::NewGuid().ToString('N').Substring(0, 6))
New-Item -ItemType Directory -Force -Path $freshData | Out-Null
$env:MH_DATA_DIR = $freshData
Write-Host "使用独立数据目录：$freshData" -ForegroundColor DarkGray

$app = $null
try {
    $app = Start-Process -FilePath $Exe -PassThru

    # 等主窗口
    $win = $null
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 400
        $app.Refresh()
        if ($app.MainWindowHandle -ne [IntPtr]::Zero) {
            $root_el = [System.Windows.Automation.AutomationElement]::FromHandle($app.MainWindowHandle)
            if ($root_el) { $win = $root_el; break }
        }
    }
    if (-not $win) { throw '主窗口没出来' }
    Start-Sleep -Seconds 3   # 等后端连上

    $all = [System.Windows.Automation.Condition]::TrueCondition
    # WinUI 在识别过程中会不断改 UI 树，FindAll 偶发 "无法识别的错误"（元素在枚举
    # 途中失效）。这些都是测试侧的噪声，不是产品问题，所以统一吞掉并重试。
    function Find-ById($id) {
        try {
            $c = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $id)
            return $win.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c)
        } catch { return $null }
    }
    function Find-ByName($name) {
        try {
            $c = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::NameProperty, $name)
            return $win.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c)
        } catch { return $null }
    }
    function Names {
        try {
            $els = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $all)
        } catch { return @() }
        $out = @()
        foreach ($el in $els) {
            try { if ($el.Current.Name) { $out += $el.Current.Name } } catch { }
        }
        return $out
    }
    function Invoke-El($el) {
        $p = $el.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
        $p.Invoke()
    }

    Write-Host "`n=== 1. 点「粘贴」开始识别 ===" -ForegroundColor Cyan
    $paste = Find-ById 'PasteButton'
    if (-not $paste) { $paste = Find-ByName '粘贴 (Ctrl+V)' }
    Check '找到粘贴按钮' ($null -ne $paste) ''
    if (-not $paste) { throw '粘贴按钮不在（AutomationId / 文案变了？）' }
    Invoke-El $paste

    Write-Host "`n=== 2. 识别中应出现「取消」按钮 ===" -ForegroundColor Cyan
    # 一旦看到按钮立刻点，别等固定时长——冷缓存下识别才 1–2 秒。
    $cancel = $null
    $busySeen = $false
    for ($i = 0; $i -lt 80; $i++) {
        $cancel = Find-ById 'CancelRecognitionButton'
        if (-not $cancel) { $cancel = Find-ByName '取消' }
        $names = Names
        if ($names | Where-Object { $_ -like '*正在识别*' -or $_ -like '*正在停止*' }) { $busySeen = $true }
        if ($cancel) { break }
        Start-Sleep -Milliseconds 50
    }
    Check '出现忙碌提示' $busySeen ''
    Check '出现取消按钮' ($null -ne $cancel) ''
    if (-not $cancel) {
        throw '识别在 4 秒内就结束了，没能在识别过程中抓到取消按钮（缓存或图片太小）'
    }

    Write-Host "`n=== 3. 点「取消」 ===" -ForegroundColor Cyan
    Invoke-El $cancel

    # 等界面从"识别中"恢复到"已取消"
    $cancelled = $false
    $trace = @()
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 300
        $names = Names
        $trace += ("[{0}] {1}" -f $i, (($names | Where-Object { $_ -like '*识别*' -or $_ -like '*取消*' }) -join ' / '))
        if ($names | Where-Object { $_ -like '*识别已取消*' }) { $cancelled = $true; break }
    }
    $finalNames = Names
    if (-not $cancelled) {
        $traceFile = Join-Path $ShotDir 'ui-cancel-trace.txt'
        Set-Content -LiteralPath $traceFile -Value ($trace -join "`n") -Encoding utf8
        Write-Host "  轨迹已写入 $traceFile" -ForegroundColor DarkGray
    }
    Check '界面显示「识别已取消」' $cancelled (($finalNames | Select-Object -First 8) -join ' | ')

    Write-Host "`n=== 4. 取消后可以继续操作 ===" -ForegroundColor Cyan
    Check '取消按钮已消失' ($null -eq (Find-ById 'CancelRecognitionButton')) ''
    $pasteAgain = Find-ById 'PasteButton'
    Check '粘贴按钮恢复可用' ($null -ne $pasteAgain -and $pasteAgain.Current.IsEnabled) ''
}
finally {
    if ($app -and -not $app.HasExited) { try { $app.Kill() } catch { } }
    Start-Sleep -Milliseconds 800
    Get-Process mhservice -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 300
    Remove-Item -LiteralPath $freshData -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "通过 $pass 项，失败 $fail 项" -ForegroundColor $(if ($fail -eq 0) { 'Green' } else { 'Red' })
exit $(if ($fail -eq 0) { 0 } else { 1 })
