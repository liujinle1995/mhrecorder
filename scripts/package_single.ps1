#!/usr/bin/env pwsh
# ============================================================================
#  package_single.ps1 — 打包成**单个 exe**（自解压式单文件）
#
#  为什么是"自解压"而不是真正的静态单文件？
#    WinUI 3 / Windows App SDK 的 DLL（Microsoft.ui.xaml.dll 等）必须与 exe
#    同目录才能被加载器找到，且本应用由两个进程组成（界面 + 后端服务）。
#    所以"一个文件"在 Windows 上只能做成：启动器 exe 内置整个应用压缩包，
#    首次运行时解压到本地私有目录再启动。用户拿到的就是一个 exe。
#
#  产物：dist\MhRecorder-<版本号>.exe（例如 dist\MhRecorder-0.9.9.exe）
#
#  用法：
#    .\scripts\package_single.ps1
#    .\scripts\package_single.ps1 -SkipUi    # 仅后端（调试用）
#    .\scripts\package_single.ps1 -KeepWork  # 保留中间目录便于排查
# ============================================================================

param(
    [switch]$SkipUi,
    [switch]$KeepWork,
    [string]$OutputName
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$scriptRoot = $PSScriptRoot

# 产物命名 = 项目名 + 版本号（版本号来自 Protocol.cs，见 scripts\version.ps1）
. (Join-Path $scriptRoot 'version.ps1')
if (-not $OutputName) { $OutputName = $SingleExeName }

function Write-Section($t) {
    Write-Host ""
    Write-Host "=== $t ===" -ForegroundColor Cyan
}

$work = Join-Path $env:TEMP ("mh-single-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$stageDir = Join-Path $work "app"
New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

try {
    # ------------------------------------------------------------------
    Write-Section '1. 准备后端与原生依赖'

    . (Join-Path $scriptRoot 'env.ps1') | Out-Null

    $mhpack = Join-Path $root 'src\target\release\mhpack.exe'
    $service = Join-Path $root 'src\target\release\mhservice.exe'
    # The GUI-subsystem stem. The shipped single file is built on top of this
    # image, so double-clicking it never allocates a console window.
    $launchStem = Join-Path $root 'src\target\release\mhlaunch.exe'
    if (-not (Test-Path $mhpack)) {
        throw "缺少 mhpack.exe，请先运行: . .\scripts\env.ps1; cd src; cargo build --release -p mhpack"
    }
    if (-not (Test-Path $launchStem)) {
        throw "缺少 mhlaunch.exe（单文件外壳），请先运行: . .\scripts\env.ps1; cd src; cargo build --release -p mhpack"
    }
    if (-not (Test-Path $service)) {
        throw "缺少 mhservice.exe，请先运行 cargo build --release"
    }

    foreach ($f in @('opencv_world4120.dll', 'onnxruntime.dll')) {
        $p = Join-Path $root "src\target\release\$f"
        if (-not (Test-Path $p)) { throw "缺少 $f（应由 build.rs 从 native\ 复制到 target\release）" }
    }

    Copy-Item $service (Join-Path $stageDir 'mhservice.exe') -Force
    foreach ($f in @('opencv_world4120.dll', 'onnxruntime.dll')) {
        Copy-Item (Join-Path $root "src\target\release\$f") (Join-Path $stageDir $f) -Force
    }

    # OCR 模型
    $modelsDst = Join-Path $stageDir 'models'
    New-Item -ItemType Directory -Force -Path $modelsDst | Out-Null
    $modelsSrc = Join-Path $root 'native\models'
    if (-not (Test-Path $modelsSrc)) { throw "缺少 native\models" }
    Get-ChildItem $modelsSrc -File | ForEach-Object { Copy-Item $_.FullName (Join-Path $modelsDst $_.Name) -Force }
    Write-Host ("  后端 + 原生依赖 + 模型：{0} MB" -f [math]::Round((Get-ChildItem $stageDir -Recurse -File | Measure-Object Length -Sum).Sum/1MB,1))

    # ------------------------------------------------------------------
    if (-not $SkipUi) {
        Write-Section '2. 发布 WinUI 界面'
        $sln = Join-Path $root 'MhRecorder\MhRecorder.sln'
        if (-not (Test-Path $sln)) { throw "缺少 $sln" }

        $uiOut = Join-Path $stageDir '.'
        & dotnet publish (Join-Path $root 'MhRecorder\MhRecorder.App\MhRecorder.App.csproj') `
            -c Release -o $uiOut -v q 2>&1 | Out-File (Join-Path $work 'publish.log') -Encoding UTF8
        if ($LASTEXITCODE -ne 0) {
            Get-Content (Join-Path $work 'publish.log') -Encoding UTF8 | Select-Object -Last 25
            throw "dotnet publish 失败（exit $LASTEXITCODE）"
        }
        Write-Host ("  界面 + 运行时：{0} MB" -f [math]::Round((Get-ChildItem $stageDir -Recurse -File | Measure-Object Length -Sum).Sum/1MB,1))
    }
    else {
        Write-Host "  已跳过界面（-SkipUi）" -ForegroundColor Yellow
    }

    # ------------------------------------------------------------------
    Write-Section '3. 清理无用文件以缩小单包'
    # 只保留中文与英文资源（游戏是中文界面）；pdbs 与日志不需要分发。
    $keep = @('zh-CN', 'zh-TW', 'en-us', 'en-GB')
    $drop = Get-ChildItem $stageDir -Directory |
        Where-Object { $_.Name -match '^[a-z]{2,3}(-[A-Za-z]{2,4})?(-[A-Za-z]{2,4})?$' -and $_.Name -notin $keep }
    $droppedMb = 0.0
    foreach ($d in $drop) {
        $droppedMb += (Get-ChildItem $d.FullName -Recurse -File | Measure-Object Length -Sum).Sum / 1MB
        Remove-Item -Recurse -Force $d.FullName
    }
    # 显式循环而不是 `... | Remove-Item`：安全删除包装（沙箱/杀软钩子）不一定
    # 支持管道输入，管道写法会报「输入对象无法绑定到该命令的任何参数」。
    foreach ($pdb in Get-ChildItem $stageDir -Recurse -File -Include *.pdb) {
        Remove-Item -LiteralPath $pdb.FullName -Force
    }
    Write-Host ("  已移除 {0} 个语言目录（{1} MB）与 pdb" -f $drop.Count, [math]::Round($droppedMb,1))

    # 单文件不保留启动器脚本：入口由 mhpack 直接调 exe
    foreach ($n in @('run.ps1', 'README.txt')) {
        Remove-Item (Join-Path $stageDir $n) -Force -ErrorAction SilentlyContinue
    }

    $stagedMb = [math]::Round((Get-ChildItem $stageDir -Recurse -File | Measure-Object Length -Sum).Sum/1MB,1)
    $stagedCount = (Get-ChildItem $stageDir -Recurse -File).Count
    Write-Host ("  待打包：{0} 个文件，{1} MB" -f $stagedCount, $stagedMb)

    # ------------------------------------------------------------------
    Write-Section '4. 生成单文件'
    $outPath = Join-Path $root "dist\$OutputName"
    New-Item -ItemType Directory -Force -Path (Split-Path $outPath) | Out-Null

    & $mhpack pack $stageDir -o $outPath --stem $launchStem
    if ($LASTEXITCODE -ne 0) { throw "mhpack pack 失败（exit $LASTEXITCODE）" }

    # ------------------------------------------------------------------
    Write-Section '5. 校验产物'
    # Verify through the console tool: the produced file is GUI-subsystem, so
    # `& $outPath --verify` would return before it printed anything.
    & $mhpack --verify $outPath
    if ($LASTEXITCODE -ne 0) { throw "单文件自校验失败" }

    $finalMb = [math]::Round((Get-Item $outPath).Length/1MB,1)

    Write-Section '完成'
    Write-Host "单文件: $outPath" -ForegroundColor Green
    Write-Host "大小  : $finalMb MB（解压后约 $stagedMb MB）" -ForegroundColor Green
    Write-Host ""
    Write-Host "分发方式：把这一个 exe 发给用户，双击即可运行。" -ForegroundColor Green
    Write-Host "首次启动会解压到 %LOCALAPPDATA%\MhRecorder\app\<版本号>\（约需数秒），" -ForegroundColor Gray
    Write-Host "之后启动秒开；数据仍存放在 %LOCALAPPDATA%\MhRecorder\mhrecorder.db。" -ForegroundColor Gray
}
finally {
    if (-not $KeepWork) {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    } else {
        Write-Host "`n中间目录保留在: $work" -ForegroundColor Yellow
    }
}
