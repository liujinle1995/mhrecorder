#!/usr/bin/env pwsh
# ============================================================================
#  package_installer.ps1 — 用 Inno Setup 把 dist\MhRecorder 打成标准安装包
#
#  产物：dist\MhRecorder-<版本>-setup.exe
#  安装向导 + 开始菜单 + 可选桌面快捷方式 + 卸载入口 + 按用户安装（免 UAC）。
#
#  前置条件：
#    1) 先跑 .\scripts\package.ps1，dist\MhRecorder 里要有完整运行时；
#    2) 需要 Inno Setup 6 的 ISCC.exe。脚本按以下顺序找：
#         环境变量 INNO_SETUP_HOME
#         build\tools\InnoSetup6            （本仓库自带的静默安装位置）
#         C:\Program Files (x86)\Inno Setup 6
#         C:\Program Files\Inno Setup 6
#       都没有就提示安装方式（winget / scoop），不猜路径。
#
#  用法：
#      .\scripts\package_installer.ps1
#      .\scripts\package_installer.ps1 -Iscc "C:\Program Files (x86)\Inno Setup 6\ISCC.exe"
# ============================================================================

[CmdletBinding()]
param(
    # 显式指定 ISCC.exe
    [string] $Iscc,

    # 只校验脚本，不产出安装包
    [switch] $DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'version.ps1')

function Write-Section($t) {
    Write-Host ''
    Write-Host "=== $t ===" -ForegroundColor Cyan
}

Write-Section "0. 版本 $AppVersion"

# --- 1. 定位 ISCC ---------------------------------------------------------
Write-Section '1. 定位 Inno Setup 编译器'

$candidates = @()
if ($Iscc) { $candidates += $Iscc }
if ($env:INNO_SETUP_HOME) { $candidates += (Join-Path $env:INNO_SETUP_HOME 'ISCC.exe') }
$candidates += (Join-Path $root 'build\tools\InnoSetup6\ISCC.exe')
$candidates += 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
$candidates += 'C:\Program Files\Inno Setup 6\ISCC.exe'

$isccPath = $candidates | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } | Select-Object -First 1
if (-not $isccPath) {
    Write-Host '找不到 ISCC.exe（Inno Setup 编译器）。' -ForegroundColor Red
    Write-Host '安装方式（任选其一）：' -ForegroundColor Yellow
    Write-Host '  winget install --id JRSoftware.InnoSetup -e'
    Write-Host '  scoop install innosetup'
    Write-Host '  或把安装目录写进 INNO_SETUP_HOME 环境变量'
    exit 1
}
Write-Host "  $isccPath" -ForegroundColor Green

# --- 2. 校验源目录与图标 ---------------------------------------------------
Write-Section '2. 校验输入'

$distDir = Join-Path $root 'dist\MhRecorder'
$appExe = Join-Path $distDir 'MhRecorder.exe'
$svcExe = Join-Path $distDir 'mhservice.exe'
$ortDll = Join-Path $distDir 'onnxruntime.dll'
$models = Join-Path $distDir 'models\ch_PP-OCRv4_rec_infer.onnx'

$missing = @()
foreach ($p in @($appExe, $svcExe, $ortDll, $models)) {
    if (-not (Test-Path -LiteralPath $p)) { $missing += $p }
}
if ($missing.Count -gt 0) {
    Write-Host '  dist\MhRecorder 不完整，先运行 .\scripts\package.ps1：' -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "    $_" -ForegroundColor Red }
    exit 1
}
foreach ($p in @($appExe, $svcExe, $ortDll)) {
    Write-Host ("  OK  {0,-22} {1,8:N1} MB" -f [System.IO.Path]::GetFileName($p), ((Get-Item $p).Length / 1MB))
}

# 图标缺失会让 ISCC 在第 30 行直接失败，先补出来。
$iconPath = Join-Path $root 'packaging\mhrecorder.ico'
if (-not (Test-Path -LiteralPath $iconPath)) {
    Write-Host '  图标缺失，先生成...' -ForegroundColor DarkYellow
    & (Join-Path $PSScriptRoot 'make_icon.ps1')
    if ($LASTEXITCODE -ne 0) { throw 'make_icon.ps1 失败' }
}

$issPath = Join-Path $root 'packaging\mhrecorder.iss'
$setupName = "$ProjectName-$AppVersion-setup.exe"
$setupPath = Join-Path $root "dist\$setupName"

# --- 3. 编译 ---------------------------------------------------------------
Write-Section '3. 编译安装包'

# 上一版同名产物被占用（正在跑安装程序）时 ISCC 会报权限错误，先说清楚。
if (Test-Path -LiteralPath $setupPath) {
    try { Remove-Item -LiteralPath $setupPath -Force }
    catch { throw "无法覆盖 $setupPath，可能它正在运行。请先关闭安装程序。" }
}

$args = @(
    "/DAppVersion=$AppVersion",
    "/Qp",              # 进度条模式，安静
    $issPath
)
Write-Host "  > ISCC.exe $($args -join ' ')"
if ($DryRun) {
    Write-Host '  -DryRun：不执行' -ForegroundColor DarkGray
    exit 0
}

& $isccPath @args
$code = $LASTEXITCODE
if ($code -ne 0) {
    Write-Host "  ISCC 失败，退出码 $code（常见原因见上方输出）" -ForegroundColor Red
    Write-Host '  提示：中文语言文件来自 Inno 主干的 ChineseSimplified.isl，' -ForegroundColor Yellow
    Write-Host '        若报 Unknown message name，说明版本不匹配，需要换成对应版本的语言文件。' -ForegroundColor Yellow
    exit $code
}

if (-not (Test-Path -LiteralPath $setupPath)) {
    throw "ISCC 报成功但没有产物：$setupPath"
}

$size = (Get-Item -LiteralPath $setupPath).Length
$sha = (Get-FileHash -LiteralPath $setupPath -Algorithm SHA256).Hash.ToLowerInvariant()

Write-Section '4. 结果'
Write-Host ("  {0,-38} {1,8:N1} MB" -f $setupName, ($size / 1MB))
Write-Host "  sha256: $sha"
Write-Host "  $setupPath"
Write-Host ''
Write-Host '安装包说明：' -ForegroundColor Green
Write-Host '  * 装到 %LOCALAPPDATA%\Programs\MhRecorder，不需要管理员权限'
Write-Host '  * 开始菜单快捷方式必建，桌面快捷方式为可选任务（默认不勾）'
Write-Host '  * 卸载默认保留 %LOCALAPPDATA%\MhRecorder 里的数据库'
Write-Host '  * 未做代码签名，首次运行会有 SmartScreen 提示（更多信息 → 仍要运行）'
exit 0
