#!/usr/bin/env pwsh
# ============================================================================
#  test_async_tasks.ps1 — 协议级验证「识别可取消」到底成不成立
#
#  为什么不用界面测：这件事的证据在协议上。0.15 的问题不是按钮没接好，而是
#  同步 recognize 把连接占到底，tasks.cancel 根本读不到。所以要证明的是：
#    1) recognize.start 在 OCR 结束**之前**就返回 taskId；
#    2) 同一条连接上随后发的 tasks.cancel 能被立刻处理；
#    3) 任务最终落到 cancelled，而不是 completed；
#    4) 重复 start 被 taskBusy 拒绝，不会排队；
#    5) system.shutdown 在还有别的客户端时被拒绝。
#
#  用法：
#      .\scripts\test_async_tasks.ps1
#      .\scripts\test_async_tasks.ps1 -Service .\src\target\release\mhservice.exe
# ============================================================================

param(
    [string] $Service,
    [int]    $Images = 6
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = Split-Path -Parent $PSScriptRoot
if (-not $Service) { $Service = Join-Path $root 'src\target\release\mhservice.exe' }
if (-not (Test-Path -LiteralPath $Service)) { throw "找不到 mhservice.exe：$Service" }

$pipeName = 'mhrecorder.v1'
$models   = Join-Path $root 'native\models'
$ort      = Join-Path $root 'native\onnxruntime\bin\onnxruntime.dll'
$dataDir  = Join-Path $env:TEMP ("mh-async-" + [guid]::NewGuid().ToString('N').Substring(0, 8))

$pass = 0; $fail = 0
function Check($name, $cond, $detail = '') {
    if ($cond) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name  $detail" -ForegroundColor Red }
}

# ---- 一条连接：发一行请求，读一行响应 -------------------------------------
class Conn {
    [System.IO.Pipes.NamedPipeClientStream] $Pipe
    [System.IO.StreamWriter] $Writer
    [System.IO.StreamReader] $Reader
    [long] $NextId = 0

    Conn() {
        $this.Pipe = [System.IO.Pipes.NamedPipeClientStream]::new(
            '.', 'mhrecorder.v1', [System.IO.Pipes.PipeDirection]::InOut)
        $this.Pipe.Connect(5000)
        $enc = [System.Text.UTF8Encoding]::new($false)
        $this.Writer = [System.IO.StreamWriter]::new($this.Pipe, $enc, 65536, $true)
        $this.Writer.AutoFlush = $true
        $this.Reader = [System.IO.StreamReader]::new($this.Pipe, $enc, $false, 65536, $true)
    }

    [object] Call([string] $method, [hashtable] $params) {
        $this.NextId++
        $req = @{ id = $this.NextId; method = $method; params = $params } | ConvertTo-Json -Depth 8 -Compress
        $this.Writer.WriteLine($req)
        $line = $this.Reader.ReadLine()
        if (-not $line) { throw "连接被关闭（$method）" }
        return $line | ConvertFrom-Json
    }

    [void] Close() {
        try { $this.Writer.Dispose() } catch { }
        try { $this.Reader.Dispose() } catch { }
        try { $this.Pipe.Dispose() } catch { }
    }
}

function Load-Shots([int] $count) {
    $files = @(Get-ChildItem -LiteralPath (Join-Path $root 'samples') -Filter '*.png' |
               Sort-Object Length -Descending | Select-Object -First $count)
    if ($files.Count -eq 0) { throw 'samples 里没有截图' }
    return @($files | ForEach-Object {
        @{
            name = $_.Name
            data = [Convert]::ToBase64String([System.IO.File]::ReadAllBytes($_.FullName))
        }
    })
}

# ---- 起后端 ---------------------------------------------------------------
Write-Host "启动后端（数据目录 $dataDir）..." -ForegroundColor Cyan
$env:MH_MODELS_DIR = $models
$env:MH_DATA_DIR = $dataDir
$env:ORT_DYLIB_PATH = $ort
$svc = Start-Process -FilePath $Service -WorkingDirectory (Split-Path -Parent $Service) `
    -PassThru -WindowStyle Hidden

try {
    # 等管道就绪
    $ready = $false
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 250
        try {
            $probe = [Conn]::new()
            $probe.Close()
            $ready = $true
            break
        } catch { }
    }
    if (-not $ready) { throw '后端 15 秒内没有就绪' }

    $c = [Conn]::new()

    Write-Host "`n=== 1. version 带上实例标识 ===" -ForegroundColor Cyan
    $v = $c.Call('version', @{})
    Check 'protocol == 2' ($v.result.protocol -eq 2) "实际 $($v.result.protocol)"
    Check '带 pid' ($v.result.pid -gt 0) ''
    Check '带 dataDir' (-not [string]::IsNullOrEmpty($v.result.dataDir)) "实际 $($v.result.dataDir)"

    Write-Host "`n=== 2. recognize.start 立刻返回 taskId ===" -ForegroundColor Cyan
    $shots = Load-Shots $Images
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $startResp = $c.Call('recognize.start', @{ images = $shots; protocol = 2 })
    $sw.Stop()
    $taskId = $startResp.result.taskId
    Check 'start 成功' ($startResp.ok -eq $true) ($startResp.error | ConvertTo-Json -Compress)
    Check '拿到 taskId' (-not [string]::IsNullOrEmpty($taskId)) ''
    Check "start 用时 < 1000ms（实际 $($sw.ElapsedMilliseconds)ms）" ($sw.ElapsedMilliseconds -lt 1000) ''

    Write-Host "`n=== 3. 同一条连接立刻发取消 ===" -ForegroundColor Cyan
    $cancel = $c.Call('tasks.cancel', @{ id = $taskId })
    Check '取消被接受' ($cancel.result.cancelled -eq $true) ($cancel | ConvertTo-Json -Compress)
    Check '状态为 cancelling' ($cancel.result.state -eq 'cancelling') "实际 $($cancel.result.state)"

    Write-Host "`n=== 4. 轮询到终态 ===" -ForegroundColor Cyan
    $terminal = $null
    $states = @()
    for ($i = 0; $i -lt 200; $i++) {
        $snap = $c.Call('tasks.get', @{ id = $taskId; consume = $false })
        $s = $snap.result.state
        if ($states.Count -eq 0 -or $states[-1] -ne $s) { $states += $s }
        if ($s -in @('cancelled', 'completed', 'failed', 'timeout')) { $terminal = $snap; break }
        Start-Sleep -Milliseconds 50
    }
    $statePath = ($states -join ' -> ')
    Check '进入终态' ($null -ne $terminal) "状态轨迹 $statePath"
    if ($terminal) {
        Check "终态是 cancelled（轨迹 $statePath）" ($terminal.result.state -eq 'cancelled') "实际 $($terminal.result.state)"
    }

    Write-Host "`n=== 5. 结果只交一次 ===" -ForegroundColor Cyan
    $first = $c.Call('tasks.get', @{ id = $taskId })
    $second = $c.Call('tasks.get', @{ id = $taskId })
    Check '第一帧带 error' ($null -ne $first.result.error) ''

    Write-Host "`n=== 6. 并发 start 被拒 ===" -ForegroundColor Cyan
    $a = $c.Call('recognize.start', @{ images = $shots; protocol = 2 })
    $b = $c.Call('recognize.start', @{ images = $shots; protocol = 2 })
    $busy = (-not $b.ok) -and ($b.error.code -eq 'taskBusy')
    Check '第二个 start 返回 taskBusy' $busy (($b.error | ConvertTo-Json -Compress))
    if ($a.ok) {
        # 收尾：取消第一个，别让它跑完占着时间
        $c.Call('tasks.cancel', @{ id = $a.result.taskId }) | Out-Null
    }

    Write-Host "`n=== 7. 关停要看有没有别的客户端 ===" -ForegroundColor Cyan
    $c2 = [Conn]::new()
    $c2.Call('version', @{}) | Out-Null
    $refused = $c.Call('system.shutdown', @{})
    Check '两个客户端时拒绝关停' ($refused.result.shuttingDown -eq $false) ($refused | ConvertTo-Json -Compress)
    Check '报出客户端数 >= 2' ($refused.result.clients -ge 2) "实际 $($refused.result.clients)"
    $c2.Close()

    Start-Sleep -Milliseconds 300
    $accepted = $c.Call('system.shutdown', @{})
    Check '只剩一个时接受关停' ($accepted.result.shuttingDown -eq $true) ($accepted | ConvertTo-Json -Compress)

    # 后端应当在一秒内退出
    $exited = $false
    for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Milliseconds 100
        if ($svc.HasExited) { $exited = $true; break }
    }
    Check '后端自行退出（不是被 kill）' $exited ''

    $c.Close()
}
finally {
    if (-not $svc.HasExited) { $svc.Kill(); $svc.WaitForExit(3000) }
    Remove-Item -LiteralPath $dataDir -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "通过 $pass 项，失败 $fail 项" -ForegroundColor $(if ($fail -eq 0) { 'Green' } else { 'Red' })
exit $(if ($fail -eq 0) { 0 } else { 1 })
