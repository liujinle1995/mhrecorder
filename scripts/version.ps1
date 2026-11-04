#!/usr/bin/env pwsh
# ============================================================================
#  version.ps1 — 版本号与产物命名的唯一来源
#
#  版本号只写在 MhRecorder.Core/Protocol.cs 的 `AppVersion` 常量的那一行，
#  打包脚本 / 发布脚本都从这里读，避免出现「exe 叫 0.9.8、界面显示 0.9.9」。
#
#  用法：在其它脚本里 dot-source 它
#      . (Join-Path $PSScriptRoot 'version.ps1')
#      # 之后可用 $ProjectName / $AppVersion
# ============================================================================

$ProjectName = 'MhRecorder'

$versionRoot = Split-Path -Parent $PSScriptRoot
$protocolPath = Join-Path $versionRoot 'MhRecorder\MhRecorder.Core\Protocol.cs'
if (-not (Test-Path -LiteralPath $protocolPath)) {
    throw "找不到版本号来源：$protocolPath"
}

$m = Select-String -LiteralPath $protocolPath -Pattern 'AppVersion\s*=\s*"([^"]+)"' |
    Select-Object -First 1
if (-not $m) { throw "Protocol.cs 里没有 AppVersion 常量：$protocolPath" }

$AppVersion = $m.Matches[0].Groups[1].Value

# 发布产物统一命名：项目名 + 版本号
$SingleExeName = "$ProjectName-$AppVersion.exe"
$PortableZipName = "$ProjectName-$AppVersion-win-x64.zip"
$InstallerName = "$ProjectName-$AppVersion-setup.exe"
$ChecksumName = "$ProjectName-$AppVersion-SHA256SUMS.txt"
