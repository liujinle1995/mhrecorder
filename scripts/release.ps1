#!/usr/bin/env pwsh
# ============================================================================
#  release.ps1 — 把 dist 里的构建产物发到 GitHub Releases
#
#  产物命名统一为「项目名 + 版本号」（版本号取自 Protocol.cs 的 AppVersion）：
#      MhRecorder-0.9.9.exe               单文件便携版（双击即用，推荐分发）
#      MhRecorder-0.9.9-win-x64.zip       免安装目录版（dist\MhRecorder 打包）
#      MhRecorder-0.9.9-setup.exe         标准安装包（Inno Setup，按用户安装）
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
    [switch]$SkipInstaller,
    [switch]$Draft,
    [switch]$Prerelease,
    [string]$Notes,
    [string]$Repo = 'liujinle1995/mhrecorder',
    [string]$Token
)

$ErrorActionPreference = 'Stop'

# GitHub 的响应是 UTF-8，而 curl.exe 的输出要经过控制台代码页转一道。
# 直接 `$out = & curl.exe ...` 再 ConvertFrom-Json，中文（release 正文里全是）
# 会被按 ANSI 解码成乱码，字节数对不上就报 "Unterminated string"。
# 让 curl 把响应写进文件，再用 UTF-8 读，绕开控制台编码。
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

# git 2.x 默认用 protocol v2，服务端响应是二进制 pkt-line 流。这个流一旦被
# PowerShell 的管道接住（`$x = & git ls-remote ...` 或 `*> file`），会被按文本
# 逐行重组，pkt-line 的边界就丢了，报 "fatal: expected flush after ref listing"
# 并以 128 退出。结果是：远端明明已经有这个 tag，脚本却判成"没有"，
# 接着去推一个已存在的 tag，再把必然失败的退出码当成"推送失败"抛出去。
# v0 的响应是纯文本，解析稳定，且 push 也仍支持，所以整体降级用 v0。
if (-not $env:GIT_CONFIG_COUNT) {
    $env:GIT_CONFIG_COUNT   = '1'
    $env:GIT_CONFIG_KEY_0   = 'protocol.version'
    $env:GIT_CONFIG_VALUE_0 = '0'
}

$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'version.ps1')

if (-not $Tag) { $Tag = "v$AppVersion" }
if ($Repo -notmatch '/') { throw "-Repo 需要 owner/name 形式" }

$dist = Join-Path $root 'dist'
$exePath = Join-Path $dist $SingleExeName
$zipPath = Join-Path $dist $PortableZipName
$setupPath = Join-Path $dist $InstallerName
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

    # 响应落盘再读，见文件头的编码说明。
    $respFile = [System.IO.Path]::GetTempFileName()
    $curlArgs = @('-sS', '--fail-with-body', '-X', $Method,
                  '-H', "Authorization: Bearer $script:tok",
                  '-H', 'Accept: application/vnd.github+json',
                  '-H', 'User-Agent: mhrecorder-release',
                  '--output', $respFile)
    if ($BodyFile) { $curlArgs += @('-H', 'Content-Type: application/json', '--data-binary', "@$BodyFile") }
    $curlArgs += $Url

    & curl.exe @curlArgs | Out-Null
    $code = $LASTEXITCODE
    $raw = if (Test-Path -LiteralPath $respFile) {
        [System.IO.File]::ReadAllText($respFile, [System.Text.UTF8Encoding]::new($false))
    } else { '' }
    Remove-Item -LiteralPath $respFile -Force -ErrorAction SilentlyContinue

    if ($code -ne 0) { throw "GitHub API 失败 ($Method $Url)：$raw" }
    if ([string]::IsNullOrWhiteSpace($raw)) { return $null }
    return ($raw | ConvertFrom-Json)
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

# 安装包：Inno Setup 编译，产物只有几十 MB 增长，但用户最认"下一步下一步装完"。
if (-not $SkipInstaller) {
    if (-not (Test-Path -LiteralPath $setupPath)) {
        Write-Host '  dist 里没有安装包，先编译...' -ForegroundColor DarkYellow
        & (Join-Path $PSScriptRoot 'package_installer.ps1')
        if ($LASTEXITCODE -ne 0) {
            Write-Host '  安装包编译失败：本次发布将不含 setup.exe' -ForegroundColor Yellow
        }
    }
}

# 校验和：给下载的人一个核对手段
$lines = @()
$checksumTargets = @($exePath, $zipPath)
if (Test-Path -LiteralPath $setupPath) { $checksumTargets += $setupPath }
foreach ($f in $checksumTargets) {
    $h = (Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash.ToLowerInvariant()
    $lines += "$h  $([System.IO.Path]::GetFileName($f))"
}
Set-Content -LiteralPath $sumPath -Value ($lines -join "`n") -Encoding ascii

foreach ($f in ($checksumTargets + $sumPath)) {
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
    # git 把进度写到 stderr；在 $ErrorActionPreference='Stop' 下会被当成错误记录抛出去，
    # 管道 `2>&1` 又会把输出搅成空串。所以直接重定向到临时文件再读，
    # 失败时至少能看到真实原因（凭据提示 / 网络 / hook 拒绝）。
    $pushLog = [System.IO.Path]::GetTempFileName()
    $env:GIT_TERMINAL_PROMPT = '0'
    & git push origin $Tag *> $pushLog
    $pushCode = $LASTEXITCODE
    $pushOut = if (Test-Path -LiteralPath $pushLog) {
        [System.IO.File]::ReadAllText($pushLog, [System.Text.UTF8Encoding]::new($false))
    } else { '' }
    Remove-Item -LiteralPath $pushLog -Force -ErrorAction SilentlyContinue

    if ($pushCode -ne 0) {
        # 再问一次远端：可能已经推上去了，只是本地退出码被 stderr 搅成了非 0
        $check = & git ls-remote --tags origin "refs/tags/$Tag" 2>$null
        if (-not $check) { throw "tag 推送失败：$pushOut" }
    }
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
# 探测"这个 tag 是否已有 Release"：首次发布必然 404，属预期，别让 curl 把
# 一行 `curl: (22) ... 404` 打到控制台末尾（看起来像发布失败）。
try { $rel = Invoke-GitHub -Method GET -Url "$api/releases/tags/$Tag" 2>$null } catch { $rel = $null }

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
if (Test-Path -LiteralPath $setupPath) {
    $want = @($want[0], [pscustomobject]@{ File = $setupPath; Asset = $InstallerName }) + $want[1..2]
}

function Send-Asset {
    param([string] $File, [string] $Query)
    # 同样落盘读，别让 curl 的 stderr 在 Stop 模式下变成异常
    $resp = [System.IO.Path]::GetTempFileName()
    & curl.exe -sS --fail-with-body -X POST `
        -H "Authorization: Bearer $script:tok" `
        -H 'Accept: application/vnd.github+json' `
        -H 'User-Agent: mhrecorder-release' `
        -H 'Content-Type: application/octet-stream' `
        --data-binary "@$File" --output $resp $Query | Out-Null
    $code = $LASTEXITCODE
    $body = if (Test-Path -LiteralPath $resp) {
        [System.IO.File]::ReadAllText($resp, [System.Text.UTF8Encoding]::new($false))
    } else { '' }
    Remove-Item -LiteralPath $resp -Force -ErrorAction SilentlyContinue
    return [pscustomobject]@{ Code = $code; Body = $body }
}

foreach ($item in $want) {
    $sizeMb = (Get-Item -LiteralPath $item.File).Length / 1MB
    # 注意：必须用 ${uploadBase} 花括号形式，`"$uploadBase?name="` 会被 $? 吃掉
    $q = "${uploadBase}?name=$([uri]::EscapeDataString($item.Asset))"
    Write-Host ("  上传 {0} ({1:N1} MB)..." -f $item.Asset, $sizeMb) -ForegroundColor Gray

    $r = Send-Asset -File $item.File -Query $q
    if ($r.Code -ne 0) {
        # 同名资产已存在时 GitHub 会 422，先删再传
        if ($r.Body -match 'already_exists') {
            Write-Host "  同名资产已存在，删除后重传" -ForegroundColor DarkYellow
            $assets = Invoke-GitHub -Method GET -Url "$api/releases/$($rel.id)/assets"
            $old = $assets | Where-Object { $_.name -eq $item.Asset } | Select-Object -First 1
            if ($old) { Invoke-GitHub -Method DELETE -Url "$api/releases/assets/$($old.id)" | Out-Null }
            $r = Send-Asset -File $item.File -Query $q
            if ($r.Code -ne 0) { throw "上传失败：$($item.Asset)`n$($r.Body)" }
        } else {
            throw "上传失败：$($item.Asset)`n$($r.Body)"
        }
    }
    Write-Host "    OK" -ForegroundColor Green
}

# ---------------------------------------------------------------------------
Write-Section '完成'
Write-Host "Release : $($rel.html_url)" -ForegroundColor Green
Write-Host "单文件  : $SingleExeName" -ForegroundColor Gray
Write-Host "安装包  : $InstallerName" -ForegroundColor Gray
Write-Host "免安装  : $PortableZipName" -ForegroundColor Gray
