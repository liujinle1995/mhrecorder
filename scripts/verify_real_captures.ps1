#!/usr/bin/env pwsh
# ============================================================================
#  verify_real_captures.ps1
#
#  Regression harness pinned to the TWO REAL in-game screenshots:
#     samples\real_character_attributes.png   (人物属性: 莫莫莫 / ID 429277361)
#     samples\real_inventory.png             (包裹: 金币/银币 icon-only row)
#
#  Every expected value below was read off the actual screenshots by a human,
#  so this script fails if the OCR/extraction heuristics regress on real data.
#
#  Also verifies the business rule the user cares about:
#    a 包裹 capture has NO identity, so it must not invent a character, and it
#    must warn the user to import 人物属性 first; once the 人物属性 is saved,
#    re-sending both screenshots must merge into the SAME character.
# ============================================================================

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
. "$root\scripts\env.ps1" | Out-Null

$mhcli = Join-Path $root "src\target\release\mhcli.exe"
$service = Join-Path $root "src\target\release\mhservice.exe"
$samples = Join-Path $root "samples"
$attr = Join-Path $samples "real_character_attributes.png"
$bag = Join-Path $samples "real_inventory.png"

foreach ($p in @($mhcli, $service)) {
    if (-not (Test-Path $p)) { Write-Host "MISSING BINARY: $p (run cargo build --release)" -ForegroundColor Red; exit 1 }
}
foreach ($p in @($attr, $bag)) {
    if (-not (Test-Path $p)) { Write-Host "MISSING SAMPLE: $p" -ForegroundColor Red; exit 1 }
}

$script:fail = 0
function Check($label, $condition, $detail = "") {
    if ($condition) { Write-Host "  PASS  $label" -ForegroundColor Green }
    else { Write-Host "  FAIL  $label   $detail" -ForegroundColor Red; $script:fail++ }
}
function Eq($label, $actual, $expected) {
    Check $label ($actual -eq $expected) "expected '$expected', got '$actual'"
}

$dataDir = Join-Path $env:TEMP ("mh-real-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Force -Path $dataDir | Out-Null
$env:MH_DATA_DIR = $dataDir

# ---------------------------------------------------------------------------
Write-Host "`n== 1. 人物属性 (real capture) ==" -ForegroundColor Cyan
$json = & $mhcli recognize $attr 2>$null | Out-String
$r = $json | ConvertFrom-Json

Eq "nickname = 莫莫莫"                       $r.character.nickname "莫莫莫"
Eq "gameId = 429277361"                     $r.character.gameId "429277361"
Eq "level = 89"                             $r.character.level 89
Eq "气血 = 5604"                            $r.stats.hp.value 5604
Eq "魔法 = 2267"                            $r.stats.mp.value 2267
Eq "物理伤害 = 709"                         $r.stats.physicalDamage.value 709
Eq "法术伤害 = 441"                         $r.stats.magicDamage.value 441
Eq "物理防御 = 1609"                        $r.stats.physicalDefense.value 1609
Eq "法术防御 = 1081"                        $r.stats.magicDefense.value 1081
Eq "速度 = 1256"                            $r.stats.speed.value 1256
Eq "治疗强度 = 133"                         $r.stats.healingPower.value 133
Check "identity resolved from the ID field" ($r.identitySource -eq "idField")
Check "no warnings on a clean panel"        ($r.warnings.Count -eq 0)
Check "no currency invented on 人物属性"    ($null -eq $r.currencies.gold -and $null -eq $r.currencies.silver)

# ---------------------------------------------------------------------------
Write-Host "`n== 2. 包裹 (real capture, icon-only currency) ==" -ForegroundColor Cyan
$json = & $mhcli recognize $bag 2>$null | Out-String
$r = $json | ConvertFrom-Json

Eq "金币 = 2625812"                         $r.currencies.gold.value 2625812
Eq "银币 = 9828458"                         $r.currencies.silver.value 9828458
Check "包裹 does NOT invent a nickname"     ($null -eq $r.character.nickname)
Check "包裹 warns to import 人物属性 first" ($r.warnings -join "" -match "请先识别")
Check "包裹 does not invent a game id"      ($null -eq $r.character.gameId)

# ---------------------------------------------------------------------------
Write-Host "`n== 3. 合并识别 (人物属性 + 包裹 in one batch) ==" -ForegroundColor Cyan
$json = & $mhcli recognize $attr $bag 2>$null | Out-String
$r = $json | ConvertFrom-Json

Eq "batch nickname"            $r.character.nickname "莫莫莫"
Eq "batch gameId"              $r.character.gameId "429277361"
Eq "batch hp"                  $r.stats.hp.value 5604
Eq "batch physicalDefense"     $r.stats.physicalDefense.value 1609
Eq "batch gold"                $r.currencies.gold.value 2625812
Eq "batch silver"              $r.currencies.silver.value 9828458
Eq "batch screenshotCount = 2" $r.screenshotCount 2
Check "merged batch has no warning" ($r.warnings.Count -eq 0)

# ---------------------------------------------------------------------------
Write-Host "`n== 4. ID-first persistence flow (the user's stated requirement) ==" -ForegroundColor Cyan
& $mhcli save $attr 2>$null | Out-Null
& $mhcli save $attr $bag 2>$null | Out-Null

$list = & $mhcli db-list 2>$null | Out-String
Check "exactly ONE character exists (bag merged, not duplicated)" ($list -match "1 characters")
Check "character is 莫莫莫"      ($list -match "莫莫莫")
Check "character has 2 snapshots" ($list -match "snapshots=2")

$stats = & $mhcli db-stats 2>$null | Out-String
$s = $stats | ConvertFrom-Json
Eq "statistics goldTotal"   $s.summary.goldTotal 2625812
Eq "statistics silverTotal" $s.summary.silverTotal 9828458
Eq "statistics characterCount" $s.summary.characterCount 1
Eq "bar nickname"  $s.characters[0].nickname "莫莫莫"

# ---------------------------------------------------------------------------
Write-Host "`n== 5. Standalone launch (must run by plain double-click) ==" -ForegroundColor Cyan
# Regression guard for a real bug: mhservice.exe died with
#   "找不到 opencv_world4120.dll"  (STATUS_DLL_NOT_FOUND / 0xC0000135)
# when started WITHOUT scripts\env.ps1 having pre-populated PATH. build.rs now
# copies the native DLLs and OCR models beside the exe, and main.rs pins
# ORT_DYLIB_PATH, so a bare launch has to work.
$releaseDir    = Join-Path $root "src\target\release"
$standaloneSvc = Join-Path $releaseDir "mhservice.exe"

Check "opencv_world4120.dll sits beside mhservice.exe" (Test-Path (Join-Path $releaseDir "opencv_world4120.dll"))
Check "onnxruntime.dll sits beside mhservice.exe"      (Test-Path (Join-Path $releaseDir "onnxruntime.dll"))
Check "OCR models sit beside mhservice.exe"            (Test-Path (Join-Path $releaseDir "models\ch_PP-OCRv4_det_infer.onnx"))

if (Test-Path $standaloneSvc) {
    # Launch with a deliberately crippled environment: no PATH help, no ORT /
    # MH_* overrides. This is exactly what a double-click looks like.
    $savedPath   = $env:Path
    $savedOrt    = $env:ORT_DYLIB_PATH
    $savedModels = $env:MH_MODELS_DIR
    $savedData   = $env:MH_DATA_DIR
    $cleanData   = Join-Path $env:TEMP ("mh-standalone-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
    try {
        $env:Path = "$env:SystemRoot\system32;$env:SystemRoot"
        Remove-Item Env:ORT_DYLIB_PATH -ErrorAction SilentlyContinue
        Remove-Item Env:MH_MODELS_DIR  -ErrorAction SilentlyContinue
        $env:MH_DATA_DIR = $cleanData

        Push-Location $releaseDir
        $out = '{"id":1,"method":"ping","params":{}}' | & $standaloneSvc --stdio 2>&1 | Out-String
        $code = $LASTEXITCODE
        Pop-Location

        Check "mhservice.exe starts with NO environment set up" ($code -eq 0) "exit=$code"
        Check "clean launch answers ping" ($out -match '"ok":true')
        Check "clean launch loads the OCR pipeline" ($out -match "OCR pipeline loaded")
    }
    finally {
        $env:Path = $savedPath
        if ($savedOrt)    { $env:ORT_DYLIB_PATH = $savedOrt } else { Remove-Item Env:ORT_DYLIB_PATH -ErrorAction SilentlyContinue }
        if ($savedModels) { $env:MH_MODELS_DIR = $savedModels } else { Remove-Item Env:MH_MODELS_DIR -ErrorAction SilentlyContinue }
        if ($savedData)   { $env:MH_DATA_DIR = $savedData } else { Remove-Item Env:MH_DATA_DIR -ErrorAction SilentlyContinue }
        Remove-Item -Recurse -Force $cleanData -ErrorAction SilentlyContinue
    }
}

# ---------------------------------------------------------------------------
Write-Host "`n== 6. Single-file build (dist\MhRecorder-单文件.exe) ==" -ForegroundColor Cyan
# The shipped artifact is one self-extracting exe. Its payload integrity and
# trailer must stay valid, otherwise end users get a corrupt app.
$single = Join-Path $root "dist\MhRecorder-单文件.exe"
if (Test-Path $single) {
    $sizeMb = [math]::Round((Get-Item $single).Length / 1MB, 1)
    Check "single-file exe exists ($sizeMb MB)" ($sizeMb -gt 50)

    # `--verify` re-hashes the embedded payload against the trailer SHA-256.
    $verifyOut = & $single --verify 2>&1 | Out-String
    Check "single-file payload self-verifies" ($LASTEXITCODE -eq 0 -and $verifyOut -match "校验通过")
} else {
    Check "single-file exe exists (run scripts\package_single.ps1)" $false
}

# ---------------------------------------------------------------------------
Write-Host ""
if ($script:fail -eq 0) {
    Write-Host "ALL REAL-CAPTURE CHECKS PASSED" -ForegroundColor Green
    Remove-Item -Recurse -Force $dataDir -ErrorAction SilentlyContinue
    exit 0
} else {
    Write-Host "$($script:fail) CHECK(S) FAILED. Artifacts kept at $dataDir" -ForegroundColor Red
    exit 1
}
