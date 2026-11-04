#!/usr/bin/env pwsh
# ============================================================================
#  test_ocr_profile.ps1 — 验证「性能档位」真的改变了引擎，而不是只写进了设置
#
#  为什么值得单独测：档位是"用户以为的"，线程数是"实际跑的"。这两者一旦
#  脱节（设置保存了但引擎没重建、或环境变量覆盖了档位），用户只会觉得
#  "这个选项没用"。所以断言全部落在 version 报回来的实际配置上。
#
#  覆盖：
#    1) 默认档位 = 平衡（4 线程 / 批 4 / DB 后处理）
#    2) 切到低资源 → 引擎就地重建，2 线程 / 批 2
#    3) 切到高性能 → 线程数 = 物理核数估算（逻辑核数 / 2，夹在 4..8）
#    4) 重建后的引擎仍然能识别（不是"配置对了但坏了"）
#    5) 设置落库，重启后档位保持
#
#  用法：.\scripts\test_ocr_profile.ps1
# ============================================================================

param(
    [string] $Service
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$PSNativeCommandUseErrorActionPreference = $false

$root = Split-Path -Parent $PSScriptRoot
if (-not $Service) { $Service = Join-Path $root 'src\target\release\mhservice.exe' }
if (-not (Test-Path -LiteralPath $Service)) { throw "找不到 mhservice.exe：$Service" }

$models = Join-Path $root 'native\models'
$ort = Join-Path $root 'native\onnxruntime\bin\onnxruntime.dll'
$dataDir = Join-Path $env:TEMP ("mh-profile-" + [guid]::NewGuid().ToString('N').Substring(0, 8))

$pass = 0; $fail = 0
function Check($name, $cond, $detail = '') {
    if ($cond) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name  $detail" -ForegroundColor Red }
}

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

    [object] Call([string] $method, [object] $params) {
        $this.NextId++
        $req = @{ id = $this.NextId; method = $method; params = $params } |
            ConvertTo-Json -Depth 8 -Compress
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

$env:MH_MODELS_DIR = $models
$env:MH_DATA_DIR = $dataDir
$env:ORT_DYLIB_PATH = $ort
Remove-Item Env:\MH_OCR_THREADS -ErrorAction SilentlyContinue
Remove-Item Env:\MH_OCR_BATCH -ErrorAction SilentlyContinue
Remove-Item Env:\MH_OCR_DET -ErrorAction SilentlyContinue

function Start-Backend {
    $p = Start-Process -FilePath $Service -WorkingDirectory (Split-Path -Parent $Service) `
        -PassThru -WindowStyle Hidden
    $ready = $false
    for ($i = 0; $i -lt 80; $i++) {
        Start-Sleep -Milliseconds 250
        try { $c = [Conn]::new(); $c.Close(); $ready = $true; break } catch { }
    }
    if (-not $ready) { throw '后端 20 秒内没有就绪' }
    return $p
}

function Stop-Backend($p) {
    try { if ($p -and -not $p.HasExited) { $p.Kill(); $p.WaitForExit(3000) | Out-Null } } catch { }
    Start-Sleep -Milliseconds 400
}

$svc = $null
try {
    Write-Host "启动后端（数据目录 $dataDir）..." -ForegroundColor Cyan
    $svc = Start-Backend
    $c = [Conn]::new()

    Write-Host "`n=== 1. 默认档位 ===" -ForegroundColor Cyan
    $v = $c.Call('version', @{})
    Check '默认 profile = balanced' ($v.result.ocr.profile -eq 'balanced') "实际 $($v.result.ocr.profile)"
    Check '4 线程' ($v.result.ocr.config.threads -eq 4) "实际 $($v.result.ocr.config.threads)"
    Check '批大小 4' ($v.result.ocr.config.batchSize -eq 4) "实际 $($v.result.ocr.config.batchSize)"
    Check '检测后处理 = db' ($v.result.ocr.config.det -eq 'db') "实际 $($v.result.ocr.config.det)"
    $cores = $v.result.ocr.cores
    Check '报出逻辑核数' ($cores -ge 1) "实际 $cores"

    $settings = ($c.Call('settings.get', @{})).result

    Write-Host "`n=== 2. 切到低资源：引擎就地重建 ===" -ForegroundColor Cyan
    $settings.ocrProfile = 'low'
    $r = $c.Call('settings.set', $settings)
    Check '保存成功' ($r.ok -eq $true) ($r | ConvertTo-Json -Compress)
    Check '报了 ocrReloaded = true' ($r.result.ocrReloaded -eq $true) "实际 $($r.result.ocrReloaded)"
    $v = $c.Call('version', @{})
    Check 'profile = low' ($v.result.ocr.profile -eq 'low') "实际 $($v.result.ocr.profile)"
    Check '2 线程' ($v.result.ocr.config.threads -eq 2) "实际 $($v.result.ocr.config.threads)"
    Check '批大小 2' ($v.result.ocr.config.batchSize -eq 2) "实际 $($v.result.ocr.config.batchSize)"

    Write-Host "`n=== 3. 切到高性能 ===" -ForegroundColor Cyan
    $settings.ocrProfile = 'high'
    $r = $c.Call('settings.set', $settings)
    Check '保存成功' ($r.ok -eq $true) ($r | ConvertTo-Json -Compress)
    $v = $c.Call('version', @{})
    $expected = [Math]::Min([Math]::Max([int]($cores / 2), 4), 8)
    Check 'profile = high' ($v.result.ocr.profile -eq 'high') "实际 $($v.result.ocr.profile)"
    Check "线程数 = $expected（物理核估算，不是 $cores 逻辑核）" ($v.result.ocr.config.threads -eq $expected) "实际 $($v.result.ocr.config.threads)"

    Write-Host "`n=== 4. 重建后的引擎仍能识别 ===" -ForegroundColor Cyan
    $shot = Join-Path $root 'samples\real_character_attributes.png'
    $img = [Convert]::ToBase64String([System.IO.File]::ReadAllBytes($shot))
    $task = $c.Call('recognize.start', @{ images = @(@{ name = 'attr.png'; data = $img }); protocol = 2 })
    Check 'start 成功' ($task.ok -eq $true) ($task | ConvertTo-Json -Compress)
    $id = $task.result.taskId
    $final = $null
    for ($i = 0; $i -lt 400; $i++) {
        $snap = $c.Call('tasks.get', @{ id = $id; consume = $false })
        if ($snap.result.state -in @('completed', 'failed', 'cancelled', 'timeout')) { $final = $snap; break }
        Start-Sleep -Milliseconds 100
    }
    Check '任务进入终态' ($null -ne $final) ''
    if ($final) {
        Check '状态 completed' ($final.result.state -eq 'completed') "实际 $($final.result.state)"
        $res = $final.result.result
        Check '识别出角色昵称 莫莫莫' ($res.character.nickname -eq '莫莫莫') "实际 $($res.character.nickname)"
        Check '血量 5604' ($res.stats.hp.value -eq 5604) "实际 $($res.stats.hp.value)"
    }
    $c.Close()

    Write-Host "`n=== 5. 设置落库：重启后档位保持 ===" -ForegroundColor Cyan
    Stop-Backend $svc
    $svc = Start-Backend
    $c = [Conn]::new()
    $v = $c.Call('version', @{})
    Check '重启后 profile = high' ($v.result.ocr.profile -eq 'high') "实际 $($v.result.ocr.profile)"
    Check "重启后线程数 = $expected" ($v.result.ocr.config.threads -eq $expected) "实际 $($v.result.ocr.config.threads)"

    $shutdown = $c.Call('system.shutdown', @{})
    Check '关停被接受' ($shutdown.ok -eq $true) ($shutdown | ConvertTo-Json -Compress)
    $c.Close()
    Start-Sleep -Milliseconds 800
}
finally {
    Stop-Backend $svc
    if (Test-Path -LiteralPath $dataDir) {
        Remove-Item -LiteralPath $dataDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Host ("`n结果：{0} 通过 / {1} 失败" -f $pass, $fail) -ForegroundColor $(if ($fail -eq 0) { 'Green' } else { 'Red' })
exit $(if ($fail -eq 0) { 0 } else { 1 })
