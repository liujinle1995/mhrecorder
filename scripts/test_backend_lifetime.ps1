#!/usr/bin/env pwsh
# ============================================================================
#  test_backend_lifetime.ps1 — 往后端自己管生死的三条规则
#
#  1. 同一数据目录只能有一个后端（第二个实例带退出码 3 退出）；
#  2. 最后一个客户端断开后，后端在宽限期内自己退出（前端崩溃也能收尾）；
#  3. 不同数据目录可以并存（各自持锁，互不干扰）。
#
#  用法： .\scripts\test_backend_lifetime.ps1
# ============================================================================

param([string] $Service)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = Split-Path -Parent $PSScriptRoot
if (-not $Service) { $Service = Join-Path $root 'src\target\release\mhservice.exe' }
if (-not (Test-Path -LiteralPath $Service)) { throw "找不到 mhservice.exe：$Service" }

$env:MH_MODELS_DIR  = Join-Path $root 'native\models'
$env:ORT_DYLIB_PATH = Join-Path $root 'native\onnxruntime\bin\onnxruntime.dll'
# 空闲退出调短一点，测试不用等 8 秒。
$env:MH_IDLE_EXIT_SEC = '3'

$pass = 0; $fail = 0
function Check($name, $cond, $detail = '') {
    if ($cond) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name  $detail" -ForegroundColor Red }
}

function Start-Backend([string] $dataDir) {
    $env:MH_DATA_DIR = $dataDir
    return Start-Process -FilePath $Service -WorkingDirectory (Split-Path -Parent $Service) `
        -PassThru -WindowStyle Hidden
}

function Connect-Once {
    $p = [System.IO.Pipes.NamedPipeClientStream]::new('.', 'mhrecorder.v1', [System.IO.Pipes.PipeDirection]::InOut)
    $p.Connect(5000)
    return $p
}

function Wait-Pipe([int] $tries = 60) {
    for ($i = 0; $i -lt $tries; $i++) {
        try { $p = Connect-Once; $p.Dispose(); return $true } catch { Start-Sleep -Milliseconds 250 }
    }
    return $false
}

$dirA = Join-Path $env:TEMP ("mh-life-a-" + [guid]::NewGuid().ToString('N').Substring(0, 6))
$dirB = Join-Path $env:TEMP ("mh-life-b-" + [guid]::NewGuid().ToString('N').Substring(0, 6))
$procs = @()

try {
    Write-Host "`n=== 1. 同一数据目录只允许一个后端 ===" -ForegroundColor Cyan
    $a1 = Start-Backend $dirA
    $procs += $a1
    Check '第一个实例起来了' (Wait-Pipe) ''

    $a2 = Start-Process -FilePath $Service -WorkingDirectory (Split-Path -Parent $Service) `
        -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $env:TEMP 'mh-life-a2.err')
    $procs += $a2
    $a2.WaitForExit(10000) | Out-Null
    Check '第二个实例自己退出了' $a2.HasExited ''
    Check '退出码是 3（实例冲突）' ($a2.ExitCode -eq 3) "实际 $($a2.ExitCode)"
    Check '第一个实例还活着' (-not $a1.HasExited) ''

    Write-Host "`n=== 2. 最后一个客户端断开后自动退出 ===" -ForegroundColor Cyan
    $c1 = Connect-Once
    Start-Sleep -Milliseconds 300
    Check '有客户端时不会退' (-not $a1.HasExited) ''
    $c1.Dispose()

    $exited = $false
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 250
        if ($a1.HasExited) { $exited = $true; break }
    }
    Check '零客户端 + 宽限期后自行退出' $exited ''
    Check '退出码是 0（正常收尾）' ($a1.ExitCode -eq 0) "实际 $($a1.ExitCode)"

    Write-Host "`n=== 3. 不同数据目录可以并存 ===" -ForegroundColor Cyan
    $b1 = Start-Backend $dirA   # 复用 A 的目录：上一个是干净退出的，锁应已释放
    $procs += $b1
    Check 'A 目录可以重新启动（锁没有残留）' (Wait-Pipe) ''
    $b2 = Start-Backend $dirB
    $procs += $b2
    Start-Sleep -Seconds 2
    Check 'B 目录的实例也活着' (-not $b2.HasExited) ''
    Check 'A 目录的实例没有被影响' (-not $b1.HasExited) ''
}
finally {
    foreach ($p in $procs) {
        if ($p -and -not $p.HasExited) {
            try { $p.Kill(); $p.WaitForExit(3000) } catch { }
        }
    }
    # 等进程真正释放锁再删目录
    Start-Sleep -Milliseconds 500
    foreach ($d in @($dirA, $dirB)) {
        Remove-Item -LiteralPath $d -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Host ""
Write-Host "通过 $pass 项，失败 $fail 项" -ForegroundColor $(if ($fail -eq 0) { 'Green' } else { 'Red' })
exit $(if ($fail -eq 0) { 0 } else { 1 })
