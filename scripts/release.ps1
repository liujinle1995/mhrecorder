#!/usr/bin/env pwsh
# ============================================================================
#  release.ps1 — 把 dist 里的构建产物发到 GitHub Releases
#
#  产物命名统一为「项目名 + 版本号」（版本号取自 Protocol.cs 的 AppVersion）：
#      MhRecorder-0.9.9.exe               单文件便携版（双击即用，推荐分发）
#      MhRecorder-0.9.9-win-x64.zip       免安装目录版（dist\MhRecorder 打包）
#      MhRecorder-0.9.9-SHA256SUMS.txt    校验和
#
#  dist/ 本身不进仓库（见 .gitignore），binary 只走 Releases，仓库保持纯源码。
#
#  用法：
#    .\scripts\release.ps1                    # 用当前版本号建 release 并上传
#    .\scripts\release.ps1 -SkipBuild         # 直接上传 dist 里已有的产物
#    .\scripts\release.ps1 -Draft             # 先存草稿，人工确认后手动发布
#    .\scripts\release.ps1 -Prerelease        # 标记为预发布
#    .\scripts\release.ps1 -Tag v0.9.10 -SkipBuild
#
#  凭据：默认从 Windows 凭据管理器（git credential fill）取，也可 -Token 显式传
#        或设置环境变量 GH_TOKEN。
# ============================================================================

param(
    [string]$Tag,
    [switch]$SkipBuild,
    [switch]$Draft,
    [switch]$Prerelease,
    [string]$Notes,
    [string]$Repo = 'liujinle1995/mhrecorder',
    [string]$Token
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'version.ps1')

if (-not $Tag) { $Tag = "v$AppVersion" }
if ($Repo -notmatch '/') { throw "-Repo 需要 owner/name 形式" }

$dist = Join-Path $root 'dist'
$exePath = Join-Path $dist $SingleExeName
$zipPath = Join-Path $dist $PortableZipName
$sumPath = Join-Path $dist $ChecksumName
$appDir = Join-Path $dist 'MhRecorder'

function Write-Section($t) {
    Write-Host ""
    Write-Host "=== $t ===" -ForegroundColor Cyan
}

function Get-GitHubToken {
    if ($Token) { return $Token }
    if ($env:GH_TOKEN) { return $env:GH_TOKEN }
    $env:GIT_TERMINAL_PROMPT = '0'
    $lines = "protocol=https`nhost=github.com`n`n" | git credential fill 2>$null
    $pw = $lines | Where-Object { $_ -like 'password=*' } | Select-Object -First 1
    if (-not $pw) {
        throw '取不到 GitHub 凭据。请传 -Token，或先让 git 记住一次 GitHub 登录凭据。'
    }
    return $pw.Substring('password='.Length)
}

function Invoke-GitHub {
    param([string]$Method, [string]$Url, [string]$BodyFile)
    $curlArgs = @('-sS', '--fail-with-body', '-X', $Method,
                  '-H', "Authorization: Bearer $script:tok",
                  '-H', 'Accept: application/vnd.github+json',
                  '-H', 'User-Agent: mhrecorder-release')
    if ($BodyFile) { $curlArgs += @('-H', 'Content-Type: application/json', '--data-binary', "@$BodyFile") }
    $curlArgs += $Url
    $out = & curl.exe @curlArgs
    if ($LASTEXITCODE -ne 0) { throw "GitHub API 失败 ($Method $Url)：$out" }
    if ([string]::IsNullOrWhiteSpace($out)) { return $null }
    return ($out | ConvertFrom-Json)
}

# 从 Changelog.cs 抽取当前版本的条目，作为 release 正文
function Get-ReleaseNotes {
    $cl = Join-Path $root 'MhRecorder\MhRecorder.Core\Services\Changelog.cs'
    if (-not (Test-Path -LiteralPath $cl)) { return '' }
    $text = Get-Content -LiteralPath $cl -Raw
    $pat = 'new\("' + [regex]::Escape($AppVersion) + '",\s*"([^"]+)",\s*new\[\]\s*\{(.*?)\}\s*\)'
    $m = [regex]::Match($text, $pat, 'Singleline')
    if (-not $m.Success) { return '' }
    $date = $m.Groups[1].Value
    $items = [regex]::Matches($m.Groups[2].Value, '"([^"]+)"') |
        ForEach-Object { '- ' + $_.Groups[1].Value }
    return "发布日期：$date`n`n## 本次变更`n`n" + ($items -join "`n")
}

Write-Section "0. 版本 $AppVersion（$Tag）"

# ---------------------------------------------------------------------------
Write-Section '1. 准备产物'

if (-not $SkipBuild) {
    if (-not (Test-Path -LiteralPath $exePath)) {
        Write-Host '  dist 里没有单文件产物，先打包...' -ForegroundColor DarkYellow
        & (Join-Path $PSScriptRoot 'package_single.ps1')
        if ($LASTEXITCODE -ne 0) { throw 'package_single.ps1 失败' }
    }
    if (-not (Test-Path -LiteralPath $appDir)) {
        Write-Host '  dist 里没有免安装目录，先打包...' -ForegroundColor DarkYellow
        & (Join-Path $PSScriptRoot 'package.ps1')
        if ($LASTEXITCODE -ne 0) { throw 'package.ps1 失败' }
    }
}

if (-not (Test-Path -LiteralPath $exePath)) {
    throw "缺少单文件产物：$exePath`n先运行 .\scripts\package_single.ps1"
}

if (-not (Test-Path -LiteralPath $zipPath)) {
    if (-not (Test-Path -LiteralPath $appDir)) {
        throw "缺少免安装目录 $appDir，无法打 zip；先运行 .\scripts\package.ps1"
    }
    Write-Host "  压缩免安装目录 -> $PortableZipName" -ForegroundColor Gray
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $appDir, $zipPath,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $true)
}

# 校验和：给下载的人一个核对手段
$lines = @()
foreach ($f in @($exePath, $zipPath)) {
    $h = (Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash.ToLowerInvariant()
    $lines += "$h  $([System.IO.Path]::GetFileName($f))"
}
Set-Content -LiteralPath $sumPath -Value ($lines -join "`n") -Encoding ascii

foreach ($f in @($exePath, $zipPath, $sumPath)) {
    Write-Host ("  {0,-40} {1,8:N1} MB" -f [System.IO.Path]::GetFileName($f), ((Get-Item $f).Length / 1MB))
}

# ---------------------------------------------------------------------------
Write-Section '2. 打 tag 并推送'

$existing = git tag -l $Tag
if ($existing) {
    Write-Host "  tag $Tag 已存在，跳过创建" -ForegroundColor DarkGray
} else {
    $subject = "MhRecorder $AppVersion"
    git tag -a $Tag -m $subject
    if ($LASTEXITCODE -ne 0) { throw "git tag 失败" }
    Write-Host "  已创建 $Tag" -ForegroundColor Green
}

# tag 已推到远端就不再重复推（重复推会以 "already exists" 失败，属正常）
$remoteTag = & git ls-remote --tags origin "refs/tags/$Tag" 2>$null
if ($remoteTag) {
    Write-Host "  远端已有 $Tag，跳过推送" -ForegroundColor DarkGray
} else {
    $pushOut = & git push origin $Tag 2>&1
    if ($LASTEXITCODE -ne 0) { throw "tag 推送失败：$pushOut" }
    Write-Host "  已推送 $Tag" -ForegroundColor Green
}

# ---------------------------------------------------------------------------
Write-Section '3. 创建 Release'

$script:tok = Get-GitHubToken
$api = "https://api.github.com/repos/$Repo"

$releaseBody = if ($Notes) { $Notes } else { Get-ReleaseNotes }
$bodyObj = [ordered]@{
    tag_name               = $Tag
    name                   = "$ProjectName $AppVersion"
    body                   = $releaseBody
    draft                  = [bool]$Draft
    prerelease             = [bool]$Prerelease
    generate_release_notes = $false
}
$bodyFile = [System.IO.Path]::GetTempFileName()
$bodyObj | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $bodyFile -Encoding utf8NoBOM

$rel = $null
try { $rel = Invoke-GitHub -Method GET -Url "$api/releases/tags/$Tag" } catch { $rel = $null }

if ($rel) {
    Write-Host "  Release $Tag 已存在（id $($rel.id)），更新正文与产物" -ForegroundColor DarkGray
    $rel = Invoke-GitHub -Method PATCH -Url "$api/releases/$($rel.id)" -BodyFile $bodyFile
} else {
    $rel = Invoke-GitHub -Method POST -Url "$api/releases" -BodyFile $bodyFile
    Write-Host "  已创建 Release（id $($rel.id)）" -ForegroundColor Green
}
Remove-Item -LiteralPath $bodyFile -Force

# ---------------------------------------------------------------------------
Write-Section '4. 上传产物'

$uploadBase = ($rel.upload_url -split '\{')[0]
$want = @(
    [pscustomobject]@{ File = $exePath; Asset = $SingleExeName },
    [pscustomobject]@{ File = $zipPath; Asset = $PortableZipName },
    [pscustomobject]@{ File = $sumPath; Asset = $ChecksumName }
)

foreach ($item in $want) {
    $sizeMb = (Get-Item -LiteralPath $item.File).Length / 1MB
    # 注意：必须用 ${uploadBase} 花括号形式，`"$uploadBase?name="` 会被 $? 吃掉
    $q = "${uploadBase}?name=$([uri]::EscapeDataString($item.Asset))"
    Write-Host ("  上传 {0} ({1:N1} MB)..." -f $item.Asset, $sizeMb) -ForegroundColor Gray
    $out = & curl.exe -sS --fail-with-body -X POST `
        -H "Authorization: Bearer $script:tok" `
        -H 'Accept: application/vnd.github+json' `
        -H 'User-Agent: mhrecorder-release' `
        -H 'Content-Type: application/octet-stream' `
        --data-binary "@$($item.File)" $q 2>&1
    if ($LASTEXITCODE -ne 0) {
        # 同名资产已存在时 GitHub 会 422，先删再传
        if ($out -match 'already_exists') {
            Write-Host "  同名资产已存在，删除后重传" -ForegroundColor DarkYellow
            $assets = Invoke-GitHub -Method GET -Url "$api/releases/$($rel.id)/assets"
            $old = $assets | Where-Object { $_.name -eq $item.Asset } | Select-Object -First 1
            if ($old) { Invoke-GitHub -Method DELETE -Url "$api/releases/assets/$($old.id)" | Out-Null }
            $out = & curl.exe -sS --fail-with-body -X POST `
                -H "Authorization: Bearer $script:tok" `
                -H 'Content-Type: application/octet-stream' `
                --data-binary "@$($item.File)" $q 2>&1
            if ($LASTEXITCODE -ne 0) { throw "上传失败：$($item.Asset)`n$out" }
        } else {
            throw "上传失败：$($item.Asset)`n$out"
        }
    }
    Write-Host "    OK" -ForegroundColor Green
}

# ---------------------------------------------------------------------------
Write-Section '完成'
Write-Host "Release : $($rel.html_url)" -ForegroundColor Green
Write-Host "单文件  : $SingleExeName" -ForegroundColor Gray
Write-Host "免安装  : $PortableZipName" -ForegroundColor Gray
