# mhrecorder — mhvision / OCR regression harness
#
# Self-contained end-to-end check for the image + OCR stack. Run it after any
# change to mhvision, the OCR models, or scripts/make_samples.py.
#
#   powershell -ExecutionPolicy Bypass -File scripts\verify_vision.ps1
#
# Optional flags:
#   -SkipBuild     reuse the existing release mhcli.exe (skip cargo build)
#   -SkipTests     skip the cargo unit-test stage
#   -KeepSamples   do not regenerate samples\ from scripts\make_samples.py
#
# Exits 0 when every check passes, 1 otherwise. Every check prints PASS/FAIL
# with the observed value so a failure can be diagnosed from the log alone.

param(
    [switch]$SkipBuild,
    [switch]$SkipTests,
    [switch]$KeepSamples
)

$ErrorActionPreference = "Stop"
# 原生命令（cargo / mhcli）往 stderr 写进度是正常的，但 Stop 模式会把它当成
# 终止错误，脚本就会在"编译进度条"这种无关输出上挂掉。
$PSNativeCommandUseErrorActionPreference = $false

$root = Split-Path -Parent $PSScriptRoot
$samples = Join-Path $root "samples"
$mhcli = Join-Path $root "src\target\release\mhcli.exe"
$script:failures = 0
$script:checks = 0

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

function Write-Head([string]$text) {
    Write-Host ""
    Write-Host "== $text ==" -ForegroundColor Cyan
}

function Assert-That {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][bool]$Condition,
        [string]$Observed = ""
    )
    $script:checks++
    if ($Condition) {
        Write-Host ("  PASS  {0}" -f $Name) -ForegroundColor Green
        if ($Observed) { Write-Host ("        {0}" -f $Observed) -ForegroundColor DarkGray }
    } else {
        $script:failures++
        Write-Host ("  FAIL  {0}" -f $Name) -ForegroundColor Red
        if ($Observed) { Write-Host ("        {0}" -f $Observed) -ForegroundColor Yellow }
    }
}

function Assert-Contains {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Haystack,
        [Parameter(Mandatory)][string]$Needle
    )
    Assert-That -Name ("{0} contains '{1}'" -f $Name, $Needle) `
                -Condition $Haystack.Contains($Needle) `
                -Observed ("{0}" -f $(if ($Haystack.Contains($Needle)) { "found" } else { "missing" }))
}

function Assert-Eq {
    param(
        [Parameter(Mandatory)][string]$Name,
        $Actual,
        $Expected
    )
    $ok = $false
    if ($null -ne $Actual -and $null -ne $Expected) {
        if ($Actual -is [string] -or $Expected -is [string]) {
            $ok = ([string]$Actual).Trim() -eq ([string]$Expected).Trim()
        } else {
            $ok = ([double]$Actual) -eq ([double]$Expected)
        }
    }
    Assert-That -Name $Name -Condition $ok `
                -Observed ("expected '{0}', got '{1}'" -f $Expected, $Actual)
}

function Assert-AtLeast {
    param(
        [Parameter(Mandatory)][string]$Name,
        $Actual,
        $Minimum
    )
    $ok = ($null -ne $Actual) -and ([double]$Actual -ge [double]$Minimum)
    Assert-That -Name $Name -Condition $ok `
                -Observed ("expected >= {0}, got '{1}'" -f $Minimum, $Actual)
}

# Run mhcli and return its stdout as a single string. stderr is dropped so
# tracing/log lines cannot corrupt JSON parsing; a non-zero exit is surfaced.
function Invoke-MhCli {
    param(
        [Parameter(Mandatory)][string[]]$MhArgs,
        [switch]$Quiet
    )
    $out = & $script:mhcli @MhArgs 2>$null
    $code = $LASTEXITCODE
    if (-not $Quiet) {
        Write-Host ("        > mhcli {0} (exit {1})" -f ($MhArgs -join ' '), $code) -ForegroundColor DarkGray
    }
    return @{ Text = ($out -join "`n"); ExitCode = $code }
}

function Invoke-MhCliJson {
    param([Parameter(Mandatory)][string[]]$MhArgs)
    $r = Invoke-MhCli -MhArgs $MhArgs
    if ($r.ExitCode -ne 0) { return $null }
    try { return ($r.Text | ConvertFrom-Json) } catch { return $null }
}

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

Write-Host "mhrecorder vision verification" -ForegroundColor Cyan
Write-Host ("  root    = {0}" -f $root)
Write-Host ("  mhcli   = {0}" -f $mhcli)

Write-Head "stage 0: environment"
if (-not (Test-Path (Join-Path $root "scripts\env.ps1"))) {
    Write-Host "  FATAL: scripts\env.ps1 not found" -ForegroundColor Red
    exit 1
}
. (Join-Path $root "scripts\env.ps1") | Out-Null
Assert-That -Name "MH_MODELS_DIR is set" -Condition (-not [string]::IsNullOrEmpty($env:MH_MODELS_DIR)) -Observed $env:MH_MODELS_DIR
Assert-That -Name "ORT_DYLIB_PATH points at the vendored onnxruntime.dll" `
            -Condition ($env:ORT_DYLIB_PATH -like "*native\onnxruntime\bin\onnxruntime.dll") `
            -Observed $env:ORT_DYLIB_PATH

$modelsDir = $env:MH_MODELS_DIR
foreach ($m in @("ch_PP-OCRv4_det_infer.onnx", "ch_PP-OCRv4_rec_infer.onnx", "ppocr_keys_v1.txt")) {
    $p = Join-Path $modelsDir $m
    Assert-That -Name ("model present: {0}" -f $m) -Condition (Test-Path $p) -Observed $p
}

# ---------------------------------------------------------------------------
# Stage 1: build
# ---------------------------------------------------------------------------

Write-Head "stage 1: build"
if ($SkipBuild) {
    Write-Host "  (skipped via -SkipBuild)" -ForegroundColor DarkGray
} else {
    Push-Location (Join-Path $root "src")
    try {
        $build = & cargo build --release -p mhcli 2>&1
        $buildOk = ($LASTEXITCODE -eq 0)
        Assert-That -Name "cargo build --release -p mhcli" -Condition $buildOk `
                    -Observed (($build | Select-Object -Last 1) -join ' ')
        if (-not $buildOk) {
            $build | Select-Object -Last 40 | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }
        }
    } finally {
        Pop-Location
    }
}
Assert-That -Name "mhcli.exe exists" -Condition (Test-Path $mhcli) -Observed $mhcli
if (-not (Test-Path $mhcli)) {
    Write-Host "  FATAL: cannot continue without mhcli.exe" -ForegroundColor Red
    exit 1
}

# ---------------------------------------------------------------------------
# Stage 2: unit tests
# ---------------------------------------------------------------------------

Write-Head "stage 2: cargo test -p mhvision"
if ($SkipTests) {
    Write-Host "  (skipped via -SkipTests)" -ForegroundColor DarkGray
} else {
    # NOTE: the workspace release profile sets `panic = "abort"`, which makes
    # the test harness unusable, so this stage deliberately runs the DEBUG
    # test profile. Do not switch this to --release.
    Push-Location (Join-Path $root "src")
    try {
        # 不能写 `cargo test ... 2>&1`：编译进度走 stderr，会被当成 ErrorRecord，
        # 在 Stop 模式下直接把脚本打断（只在需要重新编译时暴露，所以一直是
        # "偶尔失败"）。整段重定向到文件，并临时放宽 EAP。
        $logFile = Join-Path ([System.IO.Path]::GetTempPath()) "mhvision-test.log"
        $prevEap = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        & cargo test -p mhvision *> $logFile
        $testOk = ($LASTEXITCODE -eq 0)
        $ErrorActionPreference = $prevEap
        $test = Get-Content -LiteralPath $logFile -Encoding utf8
        $summary = ($test | Select-String -Pattern "test result:.*" | Select-Object -Last 1)
        Assert-That -Name "cargo test -p mhvision" -Condition $testOk `
                    -Observed ($(if ($summary) { $summary.Line.Trim() } else { "no summary line" }))
        if (-not $testOk) {
            $test | Select-Object -Last 40 | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }
        }
    } finally {
        Pop-Location
    }
}

# ---------------------------------------------------------------------------
# Stage 3: sample screenshots
# ---------------------------------------------------------------------------

Write-Head "stage 3: sample screenshots"
$required = @(
    "character_haitaotao.png",
    "character_yunzhonghe.png",
    "character_xiaolingdang.png",
    "character_qingfengxulai.png",
    "inventory.png",
    "equipment.png"
)

if ($KeepSamples) {
    Write-Host "  (keeping existing samples via -KeepSamples)" -ForegroundColor DarkGray
} else {
    $gen = & python (Join-Path $root "scripts\make_samples.py") $samples 2>&1
    $genOk = ($LASTEXITCODE -eq 0)
    Assert-That -Name "python scripts\make_samples.py" -Condition $genOk `
                -Observed (($gen | Select-Object -Last 1) -join ' ')
}
foreach ($name in $required) {
    $p = Join-Path $samples $name
    Assert-That -Name ("sample present: {0}" -f $name) -Condition (Test-Path $p) -Observed $p
}

$charShot = Join-Path $samples "character_haitaotao.png"
$invShot = Join-Path $samples "inventory.png"
$equipShot = Join-Path $samples "equipment.png"

# ---------------------------------------------------------------------------
# Stage 4: OCR — real Chinese text must come back
# ---------------------------------------------------------------------------

Write-Head "stage 4: OCR reads Chinese text"
$ocr = Invoke-MhCli -MhArgs @("ocr", $charShot)
Assert-That -Name "mhcli ocr exits 0" -Condition ($ocr.ExitCode -eq 0) -Observed ("exit {0}" -f $ocr.ExitCode)

if ($ocr.ExitCode -eq 0) {
    $ocrText = $ocr.Text
    $lineCount = 0
    if ($ocrText -match "(\d+) lines") { $lineCount = [int]$Matches[1] }
    Assert-AtLeast -Name "OCR line count" -Actual $lineCount -Minimum 20

    # The literal strings the sample renders; each must survive the full
    # det -> crop -> rec -> CTC pipeline.
    foreach ($needle in @("海淘淘", "气血", "魔法", "7,721", "金币", "2,942,685", "银币", "14,297,169", "大唐官府")) {
        Assert-Contains -Name "OCR text" -Haystack $ocrText -Needle $needle
    }
    # Confidence of every line must be sane, and at least most must be high.
    $confs = [regex]::Matches($ocrText, "^[ \t]*([0-9]{3}\.?[0-9]{3})[ \t]+\([0-9-]+,", "Multiline") |
        ForEach-Object { [double]$_.Groups[1].Value }
    if ($confs.Count -gt 0) {
        $minConf = ($confs | Measure-Object -Minimum).Minimum
        $avgConf = ($confs | Measure-Object -Average).Average
        Assert-AtLeast -Name "minimum OCR line confidence" -Actual $minConf -Minimum 0.80
        Assert-AtLeast -Name "average OCR line confidence" -Actual $avgConf -Minimum 0.95
    }
}

# ---------------------------------------------------------------------------
# Stage 5: full pipeline field extraction
# ---------------------------------------------------------------------------

Write-Head "stage 5: recognize character attributes"
$rec = Invoke-MhCliJson -MhArgs @("recognize", $charShot)
Assert-That -Name "mhcli recognize exits 0 and emits JSON" -Condition ($null -ne $rec) -Observed $(if ($rec) { "parsed" } else { "parse failed" })

if ($rec) {
    Assert-Eq -Name "nickname" -Actual $rec.character.nickname -Expected "海淘淘"
    Assert-Eq -Name "gameId" -Actual $rec.character.gameId -Expected "429277013"
    Assert-Eq -Name "level" -Actual $rec.character.level -Expected 89
    Assert-Eq -Name "school" -Actual $rec.character.school -Expected "大唐官府"

    Assert-Eq -Name "hp" -Actual $rec.stats.hp.value -Expected 7721
    Assert-Eq -Name "mp" -Actual $rec.stats.mp.value -Expected 2215
    Assert-Eq -Name "physicalDamage" -Actual $rec.stats.physicalDamage.value -Expected 758
    Assert-Eq -Name "magicDamage" -Actual $rec.stats.magicDamage.value -Expected 582
    Assert-Eq -Name "physicalDefense" -Actual $rec.stats.physicalDefense.value -Expected 1714
    Assert-Eq -Name "magicDefense" -Actual $rec.stats.magicDefense.value -Expected 948
    Assert-Eq -Name "speed" -Actual $rec.stats.speed.value -Expected 892
    Assert-Eq -Name "healingPower" -Actual $rec.stats.healingPower.value -Expected 408

    Assert-Eq -Name "gold" -Actual $rec.currencies.gold.value -Expected 2942685
    Assert-Eq -Name "silver" -Actual $rec.currencies.silver.value -Expected 14297169

    Assert-AtLeast -Name "overall confidence" -Actual $rec.confidence -Minimum 0.90
    Assert-Eq -Name "identitySource" -Actual $rec.identitySource -Expected "idField"
    Assert-Eq -Name "screenshotCount" -Actual $rec.screenshotCount -Expected 1

    Assert-AtLeast -Name "screen classified as characterAttributes" `
        -Actual $(if ($rec.screens -contains "characterAttributes") { 1 } else { 0 }) -Minimum 1
}

# ---------------------------------------------------------------------------
# Stage 6: inventory
# ---------------------------------------------------------------------------

Write-Head "stage 6: recognize inventory"
$inv = Invoke-MhCliJson -MhArgs @("recognize", $invShot)
Assert-That -Name "mhcli recognize on inventory exits 0 and emits JSON" -Condition ($null -ne $inv) -Observed $(if ($inv) { "parsed" } else { "parse failed" })

if ($inv) {
    $items = @($inv.inventory)
    Assert-AtLeast -Name "inventory item count" -Actual $items.Count -Minimum 8

    # Spot-check the pairings the extractor has to get right: name -> stack
    # count, where the count is OCR'd from a separate line below the name.
    $expected = @{
        "金柳露"   = 12
        "玄天残卷" = 3
        "九转金丹" = 8
        "金刚石"   = 24
        "百岁香"   = 14
    }
    foreach ($k in $expected.Keys) {
        $found = $items | Where-Object { $_.name -eq $k } | Select-Object -First 1
        $ok = ($null -ne $found) -and ([int]$found.count -eq [int]$expected[$k])
        Assert-That -Name ("inventory '{0}' = {1}" -f $k, $expected[$k]) -Condition $ok `
                    -Observed $(if ($found) { "got count {0}" -f $found.count } else { "item not found" })
    }
}

# ---------------------------------------------------------------------------
# Stage 7: equipment
# ---------------------------------------------------------------------------

Write-Head "stage 7: recognize equipment"
$eq = Invoke-MhCliJson -MhArgs @("recognize", $equipShot)
Assert-That -Name "mhcli recognize on equipment exits 0 and emits JSON" -Condition ($null -ne $eq) -Observed $(if ($eq) { "parsed" } else { "parse failed" })

if ($eq) {
    $slots = @($eq.equipment)
    Assert-AtLeast -Name "equipment slot count" -Actual $slots.Count -Minimum 8
    foreach ($slot in @("武器", "头盔", "衣服", "项链", "腰带", "鞋子", "戒指", "饰品")) {
        $hit = $slots | Where-Object { $_.slot -eq $slot } | Select-Object -First 1
        Assert-That -Name ("equipment slot '{0}' recognised" -f $slot) -Condition ($null -ne $hit) `
                    -Observed $(if ($hit) { "name '{0}'" -f $hit.name } else { "missing" })
    }
    $weapon = $slots | Where-Object { $_.slot -eq "武器" } | Select-Object -First 1
    if ($weapon) {
        Assert-Eq -Name "weapon name" -Actual $weapon.name -Expected "霜冷九州"
    }
}

# ---------------------------------------------------------------------------
# Stage 8: perceptual features / similarity
# ---------------------------------------------------------------------------

Write-Head "stage 8: features and similarity"
$feat = Invoke-MhCli -MhArgs @("features", $charShot)
Assert-That -Name "mhcli features exits 0" -Condition ($feat.ExitCode -eq 0) -Observed ("exit {0}" -f $feat.ExitCode)
if ($feat.ExitCode -eq 0) {
    $hasHash = $feat.Text -match "phash\s*=\s*([0-9a-f]{16})"
    Assert-That -Name "phash is a 64-bit hex value" -Condition $hasHash -Observed $(if ($hasHash) { $Matches[1] } else { "not found" })
    $orbDesc = 0
    if ($feat.Text -match "(\d+) descriptor bytes") { $orbDesc = [int]$Matches[1] }
    Assert-AtLeast -Name "ORB descriptor bytes (32 per keypoint)" -Actual $orbDesc -Minimum 32
    Assert-That -Name "ORB descriptor bytes are a multiple of 32" -Condition ($orbDesc % 32 -eq 0) -Observed ("{0} bytes" -f $orbDesc)
    Assert-That -Name "HSV histogram is printed" -Condition ($feat.Text -match "hsv\(32 bin\)") -Observed "hsv(32 bin) line"
}

$cmpSame = Invoke-MhCli -MhArgs @("compare", $charShot, $charShot)
Assert-That -Name "mhcli compare (same image) exits 0" -Condition ($cmpSame.ExitCode -eq 0) -Observed ("exit {0}" -f $cmpSame.ExitCode)
if ($cmpSame.ExitCode -eq 0) {
    $blend = $null
    if ($cmpSame.Text -match "blend\s*=\s*([0-9.]+)") { $blend = [double]$Matches[1] }
    Assert-Eq -Name "self-compare blend" -Actual $blend -Expected 1.0
    $orbSelf = $null
    if ($cmpSame.Text -match "orb\s*=\s*([0-9.]+)") { $orbSelf = [double]$Matches[1] }
    Assert-Eq -Name "self-compare orb" -Actual $orbSelf -Expected 1.0
}

$cmpDiff = Invoke-MhCli -MhArgs @("compare", $charShot, $invShot)
Assert-That -Name "mhcli compare (different images) exits 0" -Condition ($cmpDiff.ExitCode -eq 0) -Observed ("exit {0}" -f $cmpDiff.ExitCode)
if ($cmpDiff.ExitCode -eq 0) {
    $blendDiff = $null
    if ($cmpDiff.Text -match "blend\s*=\s*([0-9.]+)") { $blendDiff = [double]$Matches[1] }
    # Different panels must NOT look identical. A loose ceiling keeps this
    # robust across font/AA differences while still catching a broken metric.
    Assert-That -Name "different images blend below 0.90" -Condition ($blendDiff -lt 0.90) `
                -Observed ("blend {0}" -f $blendDiff)
}

# ---------------------------------------------------------------------------
# Stage 9: multi-screenshot batch merge
# ---------------------------------------------------------------------------

Write-Head "stage 9: batch merge"
$otherShot = Join-Path $samples "character_yunzhonghe.png"
$batch = Invoke-MhCliJson -MhArgs @("recognize", $charShot, $otherShot)
Assert-That -Name "mhcli recognize (batch of 2) exits 0 and emits JSON" -Condition ($null -ne $batch) -Observed $(if ($batch) { "parsed" } else { "parse failed" })
if ($batch) {
    Assert-Eq -Name "batch screenshotCount" -Actual $batch.screenshotCount -Expected 2
    Assert-AtLeast -Name "batch recorded both screens" -Actual @($batch.screens).Count -Minimum 2
    Assert-Eq -Name "batch keeps first character's hp" -Actual $batch.stats.hp.value -Expected 7721
    Assert-Eq -Name "batch keeps first character's gold" -Actual $batch.currencies.gold.value -Expected 2942685
}

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

$passed = $script:checks - $script:failures
Write-Host ""
Write-Host ("{0}/{1} checks passed, {2} failed" -f $passed, $script:checks, $script:failures) `
    -ForegroundColor $(if ($script:failures -eq 0) { "Green" } else { "Red" })

if ($script:failures -eq 0) {
    Write-Host "RESULT: PASS" -ForegroundColor Green
    exit 0
} else {
    Write-Host "RESULT: FAIL" -ForegroundColor Red
    exit 1
}
