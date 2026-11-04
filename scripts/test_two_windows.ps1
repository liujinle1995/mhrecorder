#!/usr/bin/env pwsh
# ============================================================================
#  test_two_windows.ps1 — 两个窗口共用一个后端时，互不影响
#
#  这正是 0.15 报告的"问题 A"：A 窗口拉起后端、B 窗口连上同一个后端，
#  A 退出时按自己的 _spawned 把后端杀掉，B 的连接随之断掉。
#
#  现在的约定：关窗口 = 断开连接 + 发 system.shutdown；后端看到还有别的客户端
#  就拒绝退出，只剩最后一个时才走。所以两个窗口可以先关任意一个。
#
#  用法： .\scripts\test_two_windows.ps1
# ============================================================================

param([string] $Exe)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = Split-Path -Parent $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $root 'dist\MhRecorder\MhRecorder.exe' }
if (-not (Test-Path -LiteralPath $Exe)) { throw "找不到界面程序：$Exe（先跑 scripts\package.ps1）" }

$pass = 0; $fail = 0
function Check($name, $cond, $detail = '') {
    if ($cond) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name  $detail" -ForegroundColor Red }
}

$sig = @'
using System;
using System.Runtime.InteropServices;
public class Wnd {
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
}
'@
Add-Type -TypeDefinition $sig | Out-Null

function Close-Window($proc) {
    $proc.Refresh()
    if ($proc.MainWindowHandle -ne [IntPtr]::Zero) {
        [Wnd]::PostMessage($proc.MainWindowHandle, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    }
    for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Milliseconds 250
        $proc.Refresh()
        if ($proc.HasExited) { return $true }
    }
    return $false
}

function Backend-Count {
    return @(Get-Process mhservice -ErrorAction SilentlyContinue).Count
}

Get-Process mhservice, MhRecorder -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 800

$app1 = $null; $app2 = $null
try {
    Write-Host "`n=== 1. 第一个窗口拉起后端 ===" -ForegroundColor Cyan
    $app1 = Start-Process -FilePath $Exe -PassThru
    $up = $false
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 400
        if ((Backend-Count) -ge 1) { $up = $true; break }
    }
    Check '后端起来了' $up ''
    Start-Sleep -Seconds 2

    Write-Host "`n=== 2. 第二个窗口复用同一个后端 ===" -ForegroundColor Cyan
    $app2 = Start-Process -FilePath $Exe -PassThru
    Start-Sleep -Seconds 5
    Check '第二个窗口没有拉起第二个后端' ((Backend-Count) -eq 1) "实际 $(Backend-Count) 个"
    Check '两个界面都还活着' ((-not $app1.HasExited) -and (-not $app2.HasExited)) ''

    Write-Host "`n=== 3. 先关第二个窗口：后端必须留下 ===" -ForegroundColor Cyan
    $closed2 = Close-Window $app2
    Check '第二个窗口正常关闭' $closed2 ''
    Start-Sleep -Seconds 2
    Check '后端仍然在跑（还有窗口连着）' ((Backend-Count) -eq 1) "实际 $(Backend-Count) 个"
    Check '第一个窗口没受影响' (-not $app1.HasExited) ''

    Write-Host "`n=== 4. 关掉最后一个窗口：后端自己退出 ===" -ForegroundColor Cyan
    $closed1 = Close-Window $app1
    Check '第一个窗口正常关闭' $closed1 ''
    $gone = $false
    for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Milliseconds 250
        if ((Backend-Count) -eq 0) { $gone = $true; break }
    }
    Check '后端在 10 秒内自己退出' $gone "实际 $(Backend-Count) 个"
}
finally {
    foreach ($p in @($app1, $app2)) {
        if ($p -and -not $p.HasExited) { try { $p.Kill() } catch { } }
    }
    Get-Process mhservice -ErrorAction SilentlyContinue | Stop-Process -Force
}

Write-Host ""
Write-Host "通过 $pass 项，失败 $fail 项" -ForegroundColor $(if ($fail -eq 0) { 'Green' } else { 'Red' })
exit $(if ($fail -eq 0) { 0 } else { 1 })
