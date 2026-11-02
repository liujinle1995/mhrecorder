#!/usr/bin/env pwsh
# End-to-end smoke test of the mhservice named pipe using only PowerShell.
# Sends recognize.save -> characters.list -> statistics.get -> exports and
# verifies the on-disk artifacts. Exits nonzero on any failure.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
. "$root\scripts\env.ps1" | Out-Null

$dataDir = Join-Path $env:TEMP ("mh-e2e-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Force -Path $dataDir | Out-Null
$env:MH_DATA_DIR = $dataDir

$service = Join-Path $root "src\target\release\mhservice.exe"
if (-not (Test-Path $service)) { throw "mhservice.exe not built: $service" }

$failures = 0
function Check($label, $condition, $detail = "") {
    if ($condition) {
        Write-Host "  PASS  $label" -ForegroundColor Green
    } else {
        Write-Host "  FAIL  $label  $detail" -ForegroundColor Red
        $script:failures++
    }
}

Write-Host "`n=== starting mhservice (pipe: mhrecorder.v1) ===" -ForegroundColor Cyan
$proc = Start-Process -FilePath $service -PassThru -NoNewWindow -RedirectStandardOutput "$dataDir\svc.out" -RedirectStandardError "$dataDir\svc.err"
Start-Sleep -Milliseconds 2500

if ($proc.HasExited) {
    Write-Host "service exited early:" -ForegroundColor Red
    Get-Content "$dataDir\svc.err" -ErrorAction SilentlyContinue | Select-Object -Last 10
    if (Test-Path "$dataDir\svc.out") { Get-Content "$dataDir\svc.out" | Select-Object -Last 5 }
    throw "mhservice failed to start"
}

$pipe = New-Object System.IO.Pipes.NamedPipeClientStream(".", "mhrecorder.v1", [System.IO.Pipes.PipeDirection]::InOut)
try {
    $pipe.Connect(8000)
    Write-Host "  connected to pipe" -ForegroundColor Green
} catch {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    throw "could not connect to pipe mhrecorder.v1: $_"
}

$reader = New-Object System.IO.StreamReader($pipe, [System.Text.Encoding]::UTF8)
$writer = New-Object System.IO.StreamWriter($pipe, (New-Object System.Text.UTF8Encoding($false)))
$writer.AutoFlush = $true

function Call($id, $method, $paramsObj) {
    $payload = @{ id = $id; method = $method; params = $paramsObj } | ConvertTo-Json -Compress -Depth 20
    $writer.WriteLine($payload)
    $line = $reader.ReadLine()
    if (-not $line) { throw "no response for $method" }
    return ($line | ConvertFrom-Json)
}

try {
    Write-Host "`n=== protocol ===" -ForegroundColor Cyan
    $ping = Call 1 "ping" @{}
    Check "ping ok=true" ($ping.ok -eq $true)
    Check "ping status=ready" ($ping.result.status -eq "ready")

    $ver = Call 2 "version" @{}
    Check "version protocol=1" ($ver.result.protocol -eq 1)
    Check "version models=ppocrv4" ($ver.result.models -eq "ppocrv4")

    Write-Host "`n=== error handling ===" -ForegroundColor Cyan
    $bad = Call 3 "no.such.method" @{}
    Check "unknown method rejected" ($bad.ok -eq $false -and $bad.error.code -eq "badRequest")
    Check "unknown method keeps id" ($bad.id -eq 3)

    $nochar = Call 4 "characters.get" @{ id = "does-not-exist" }
    Check "missing character -> notFound" ($nochar.ok -eq $false -and $nochar.error.code -eq "notFound")

    $noimgs = Call 5 "recognize" @{ images = @() }
    # If the OCR models failed to load the backend rejects up front with ocrFailed;
    # otherwise an empty batch is a badRequest. Either is a correct rejection.
    Check "empty images rejected" ($noimgs.ok -eq $false -and $noimgs.error.code -in @("badRequest","ocrFailed")) "code=$($noimgs.error.code)"

    Write-Host "`n=== persistence round-trip ===" -ForegroundColor Cyan
    $fake = @{
        characterId = $null
        character   = @{ nickname = "海淘淘"; gameId = "429277013"; level = 89; school = "大唐官府"; server = "长安城" }
        screens     = @("characterAttributes")
        stats       = @{
            hp = @{ value = 7721; confidence = 0.99; rawText = "7,721" }
            mp = @{ value = 2215; confidence = 0.98; rawText = "2,215" }
            physicalDamage = @{ value = 758; confidence = 0.97; rawText = "758" }
            magicDamage = @{ value = 582; confidence = 0.97; rawText = "582" }
            physicalDefense = @{ value = 1714; confidence = 0.96; rawText = "1,714" }
            magicDefense = @{ value = 948; confidence = 0.97; rawText = "948" }
            speed = @{ value = 892; confidence = 0.98; rawText = "892" }
            healingPower = @{ value = 408; confidence = 0.96; rawText = "408" }
        }
        currencies  = @{
            gold = @{ value = 2942685; confidence = 0.99; rawText = "2,942,685" }
            silver = @{ value = 14297169; confidence = 0.99; rawText = "14,297,169" }
        }
        equipment   = @()
        inventory   = @()
        confidence  = 0.96
        identitySource = "idField"
        warnings    = @()
        screenshotCount = 2
        cacheHits   = @()
    }

    $save = Call 6 "recognize.save" @{ result = $fake; screens = @("character_haitaotao.png", "inventory.png") }
    Check "recognize.save ok" ($save.ok -eq $true)
    Check "recognize.save returned id" ($null -ne $save.result.characterId)
    $charId = $save.result.characterId

    $list = Call 7 "characters.list" @{}
    Check "list has 1 character" ($list.result.characters.Count -eq 1)
    $c0 = $list.result.characters[0]
    Check "nickname persisted" ($c0.character.nickname -eq "海淘淘")
    Check "gameId persisted" ($c0.character.gameId -eq "429277013")
    Check "snapshotCount=1" ($c0.snapshotCount -eq 1)
    Check "gold persisted" ($c0.character.latestCurrencies.gold.value -eq 2942685)

    # Saving again with the same gameId must update, not duplicate.
    $save2 = Call 8 "recognize.save" @{ result = $fake; screens = @("again.png") }
    Check "second save same id" ($save2.result.characterId -eq $charId)
    $list2 = Call 9 "characters.list" @{}
    Check "no duplicate character" ($list2.result.characters.Count -eq 1)
    Check "snapshotCount=2" ($list2.result.characters[0].snapshotCount -eq 2)

    $get = Call 10 "characters.get" @{ id = $charId }
    Check "characters.get works" ($get.ok -eq $true -and $get.result.character.id -eq $charId)

    $upd = Call 11 "characters.update" @{ id = $charId; nickname = "海涛涛"; gameId = "429277013"; level = 90 }
    Check "characters.update ok" ($upd.ok -eq $true)
    $get2 = Call 12 "characters.get" @{ id = $charId }
    Check "correction applied" ($get2.result.character.nickname -eq "海涛涛")
    Check "correction confirmed identity" ($get2.result.character.identityConfirmed -eq $true)
    Check "correction set confidence 1.0" ($get2.result.character.identityConfidence -eq 1.0)

    Write-Host "`n=== statistics ===" -ForegroundColor Cyan
    $stats = Call 13 "statistics.get" @{ recentLimit = 5 }
    Check "stats ok" ($stats.ok -eq $true)
    Check "goldTotal correct" ($stats.result.summary.goldTotal -eq 2942685)
    Check "silverTotal correct" ($stats.result.summary.silverTotal -eq 14297169)
    Check "characterCount=1" ($stats.result.summary.characterCount -eq 1)
    Check "one bar group" ($stats.result.characters.Count -eq 1)
    Check "bar gold in 万元" ([math]::Abs($stats.result.characters[0].goldWan - 294.2685) -lt 0.001)

    Write-Host "`n=== settings ===" -ForegroundColor Cyan
    $set = Call 14 "settings.set" @{ editableBeforeSave = $false; autoNavigateAfterSave = $true; dataDir = $dataDir }
    Check "settings.set ok" ($set.ok -eq $true)
    $setg = Call 15 "settings.get" @{}
    Check "toggle persisted" ($setg.result.editableBeforeSave -eq $false)
    Check "autoNavigate persisted" ($setg.result.autoNavigateAfterSave -eq $true)

    Write-Host "`n=== exports ===" -ForegroundColor Cyan
    $jsonPath = Join-Path $dataDir "export.json"
    $ej = Call 16 "export.json" @{ path = $jsonPath }
    Check "export.json ok" ($ej.ok -eq $true)
    Check "export.json file exists" (Test-Path $jsonPath)
    if (Test-Path $jsonPath) {
        $doc = Get-Content $jsonPath -Raw | ConvertFrom-Json
        Check "export json has characters" ($doc.characters.Count -eq 1)
        Check "export json has snapshots" ($doc.snapshots.Count -eq 2)
    }

    $csvPath = Join-Path $dataDir "characters.csv"
    $ec = Call 17 "export.csv" @{ path = $csvPath }
    Check "export.csv ok" ($ec.ok -eq $true)
    Check "export.csv file exists" (Test-Path $csvPath)
    if (Test-Path $csvPath) {
        $rows = Get-Content $csvPath
        Check "csv has header+1 row" ($rows.Count -eq 2)
        Check "csv contains nickname" ($rows[1] -match "海涛涛")
    }

    $zipPath = Join-Path $dataDir "export.zip"
    $ez = Call 18 "export.zip" @{ path = $zipPath }
    Check "export.zip ok" ($ez.ok -eq $true)
    Check "export.zip exists" (Test-Path $zipPath)
    if (Test-Path $zipPath) {
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
        $entry = $zip.Entries | Where-Object { $_.Name -eq "data.json" }
        Check "zip contains data.json" ($null -ne $entry)
        $zip.Dispose()
    }

    Write-Host "`n=== data.clear ===" -ForegroundColor Cyan
    $clr = Call 19 "data.clear" @{}
    Check "data.clear ok" ($clr.ok -eq $true)
    $empty = Call 20 "characters.list" @{}
    Check "characters gone after clear" ($empty.result.characters.Count -eq 0)
    $setg2 = Call 21 "settings.get" @{}
    Check "settings survive clear" ($setg2.result.autoNavigateAfterSave -eq $true)

    Write-Host "`n=== framing (multiple calls on one connection) ===" -ForegroundColor Cyan
    $seq = @()
    foreach ($i in 22..26) { $seq += (Call $i "ping" @{}).id }
    Check "sequential ids intact" (($seq -join ",") -eq "22,23,24,25,26")
}
finally {
    $writer.Dispose()
    $reader.Dispose()
    $pipe.Dispose()
    if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 300
}

Write-Host ""
if ($failures -eq 0) {
    Write-Host "ALL PIPE E2E TESTS PASSED" -ForegroundColor Green
    Remove-Item -Recurse -Force $dataDir -ErrorAction SilentlyContinue
    exit 0
} else {
    Write-Host "$failures FAILURE(S). Artifacts kept at $dataDir" -ForegroundColor Red
    exit 1
}
