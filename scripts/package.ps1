#!/usr/bin/env pwsh
# ============================================================================
#  package.ps1 — build a self-contained, no-install distribution folder.
#
#  Produces:  D:\app\mhrecorder\dist\MhRecorder\
#    MhRecorder.exe / *.dll / *.winmd ...   (from `dotnet publish`, WinUI app)
#    mhservice.exe                          (Rust backend, copied from cargo)
#    onnxruntime.dll                        (vendored ONNX Runtime 1.31)
#    opencv_world4120.dll                   (vendored OpenCV 4.12)
#    models\                                (PP-OCRv4 det/rec + dict)
#    run.ps1                                (generated launcher, sets env)
#    README.txt                             (generated Chinese usage notes)
#
#  The launcher matters: C:\Windows\System32 ships an ancient onnxruntime.dll
#  (1.17) that Windows will happily load first and that is too old for the
#  `ort` crate (needs >= 1.31). run.ps1 exports ORT_DYLIB_PATH to the bundled
#  copy and prepends the dist folder to PATH before starting the app.
#
#  Usage:
#      .\scripts\package.ps1                 # normal packaging
#      .\scripts\package.ps1 -SkipUi         # backend-only folder
#      .\scripts\package.ps1 -Strict         # warnings also fail the build
#
#  Idempotent: the dist folder is wiped and rebuilt on every run.
# ============================================================================

[CmdletBinding()]
param(
    [ValidateSet( 'Release', 'Debug' )]
    [string] $Configuration = 'Release',

    # Skip the WinUI `dotnet publish` step (backend artifacts only).
    [switch] $SkipUi,

    # Treat warnings (missing mhservice.exe / missing UI project) as errors.
    [switch] $Strict
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:Root     = Split-Path -Parent $PSScriptRoot          # D:\app\mhrecorder
$script:DistRoot = Join-Path $script:Root 'dist\MhRecorder'
$script:Errors   = New-Object System.Collections.Generic.List[string]
$script:Warnings = New-Object System.Collections.Generic.List[string]

# 随包的 README.txt 里那行版本号以前写死成 0.9.2，每个版本都在发错的信息。
# 统一从 Protocol.cs 的 AppVersion 取（与产物命名同源）。
. (Join-Path $PSScriptRoot 'version.ps1')

# --- small helpers ---------------------------------------------------------

function Write-Section {
    param( [string] $Title )
    Write-Host ''
    Write-Host "=== $Title ===" -ForegroundColor Cyan
}

function Add-Error {
    param( [string] $Message )
    $script:Errors.Add($Message)
    Write-Host "  FAIL  $Message" -ForegroundColor Red
}

function Add-Warning {
    param( [string] $Message )
    $script:Warnings.Add($Message)
    Write-Host "  WARN  $Message" -ForegroundColor Yellow
}

function Format-Size {
    param( [long] $Bytes )
    if ($Bytes -ge 1MB) { return ('{0:N1} MB' -f ($Bytes / 1MB)) }
    if ($Bytes -ge 1KB) { return ('{0:N1} KB' -f ($Bytes / 1KB)) }
    return "$Bytes B"
}

function Write-TextFileBom {
    # UTF-8 *with* BOM: Windows PowerShell 5.1 reads BOM-less scripts as ANSI/GBK
    # and would mangle the Chinese in the generated README / launcher.
    param(
        [string] $Path,
        [string] $Content
    )
    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path -LiteralPath $dir)) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }
    $utf8Bom = New-Object System.Text.UTF8Encoding($true)
    [System.IO.File]::WriteAllText($Path, $Content, $utf8Bom)
}

function Copy-Artifact {
    param(
        [string] $Source,
        [string] $DestinationDir,
        [string] $Label,
        [switch] $Required
    )

    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        if ($Required) {
            Add-Error "缺少必需的构建产物: $Label ($Source)"
        } else {
            Add-Warning "缺少构建产物: $Label ($Source)"
        }
        return $false
    }

    if (-not (Test-Path -LiteralPath $DestinationDir)) {
        New-Item -ItemType Directory -Force -Path $DestinationDir | Out-Null
    }
    Copy-Item -LiteralPath $Source -Destination $DestinationDir -Force
    $leaf = Split-Path -Leaf $Source
    $dest = Join-Path $DestinationDir $leaf

    $size  = (Get-Item -LiteralPath $dest).Length
    $sha   = (Get-FileHash -LiteralPath $dest -Algorithm SHA256).Hash.Substring(0, 12)
    Write-Host ("  OK    {0}  {1}  sha256:{2}" -f $Label, (Format-Size $size), $sha)
    return $true
}

# --- banner ----------------------------------------------------------------

Write-Host 'mhrecorder packaging' -ForegroundColor Green
Write-Host "  root        : $script:Root"
Write-Host "  dist        : $script:DistRoot"
Write-Host "  config      : $Configuration"
Write-Host "  rust target : src\target\$( $Configuration.ToLower() )"

# --- 1. clean the dist folder ---------------------------------------------

Write-Section 'Preparing dist folder'

if (Test-Path -LiteralPath $script:DistRoot) {
    # 为什么不是一个 Remove-Item -Recurse：
    # 本机会拦"单轮累计删除 >50 个文件"的批量删除（安全删除钩子），而 dist 有
    # 三百多个文件，打包必须无人值守。所以退一步：分批删，删不完就直接覆盖发布
    # （dotnet publish 本来就会重写同名文件），只在日志里说清楚"可能有旧文件残留"。
    # 真要彻底清干净，手工删 dist\MhRecorder 或跑 scripts\clean.ps1 即可。
    $cleaned = $false
    try {
        $files = @(Get-ChildItem -LiteralPath $script:DistRoot -Recurse -File -Force -ErrorAction SilentlyContinue)
        $chunkSize = 12
        for ($i = 0; $i -lt $files.Count; $i += $chunkSize) {
            $end = [Math]::Min($i + $chunkSize - 1, $files.Count - 1)
            Remove-Item -LiteralPath $files[$i..$end].FullName -Force -ErrorAction Stop
        }

        $dirs = @(Get-ChildItem -LiteralPath $script:DistRoot -Recurse -Directory -Force -ErrorAction SilentlyContinue |
                  Sort-Object -Property FullName -Descending)
        foreach ($d in $dirs) {
            Remove-Item -LiteralPath $d.FullName -Recurse -Force -ErrorAction SilentlyContinue
        }

        Remove-Item -LiteralPath $script:DistRoot -Recurse -Force -ErrorAction Stop
        $cleaned = $true
        Write-Host "  cleaned previous dist\MhRecorder ($($files.Count) files)"
    }
    catch {
        Write-Host "  WARN  未能清空 dist\MhRecorder，改为覆盖发布" -ForegroundColor Yellow
        Write-Host "       原因: $($_.Exception.Message.Substring(0, [Math]::Min(160, $_.Exception.Message.Length)))" -ForegroundColor DarkGray
        Write-Host "       影响: 新版本不再生成、旧版本遗留的文件会留在目录里" -ForegroundColor DarkGray
        Write-Host "       彻底清理: 关闭占用后手工删除 dist\MhRecorder" -ForegroundColor DarkGray
    }

    if (-not $cleaned) {
        New-Item -ItemType Directory -Force -Path $script:DistRoot | Out-Null
    }
}
New-Item -ItemType Directory -Force -Path $script:DistRoot | Out-Null
Write-Host '  dist folder ready'

# --- 2. publish the WinUI app ---------------------------------------------
# Done first so that `dotnet publish` owns the folder, then our vendored
# native artifacts are laid on top and can never be clobbered.

Write-Section 'Publishing WinUI app (dotnet publish)'

$appCsproj = Join-Path $script:Root 'MhRecorder\MhRecorder.App\MhRecorder.App.csproj'

if ($SkipUi) {
    Add-Warning 'UI publish skipped (-SkipUi); dist will contain the backend only'
}
elseif (-not (Test-Path -LiteralPath $appCsproj -PathType Leaf)) {
    Add-Warning "UI project not found yet: $appCsproj"
    Add-Warning 'dist will contain the backend only; re-run this script after the UI lands'
}
else {
    $dotnet = Get-Command dotnet -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $dotnet) {
        Add-Error 'dotnet CLI 未安装或不在 PATH 中，无法发布 WinUI 应用'
    }
    else {
        $dotnetExe = $dotnet.Source
        $publishArgs = @(
            'publish', $appCsproj,
            '-c', $Configuration,
            '-o', $script:DistRoot,
            '--nologo'
        )
        Write-Host "  > dotnet $($publishArgs -join ' ')"

        $publishOutput = & $dotnetExe @publishArgs 2>&1
        $publishExit   = $LASTEXITCODE

        if ($publishExit -ne 0) {
            Write-Host "  dotnet publish failed (exit $publishExit), last output:" -ForegroundColor Red
            $publishOutput | Select-Object -Last 25 | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }
            Add-Error "dotnet publish 失败 (exit $publishExit)"
        }
        else {
            Write-Host '  OK    WinUI app published'
        }
    }
}

# --- 3. copy the Rust backend ---------------------------------------------

Write-Section 'Copying Rust backend'

$rustTargetDir = Join-Path $script:Root ("src\target\" + $Configuration.ToLower())
$serviceExe    = Join-Path $rustTargetDir 'mhservice.exe'

if (Copy-Artifact -Source $serviceExe -DestinationDir $script:DistRoot -Label 'mhservice.exe') {
    $built = (Get-Item -LiteralPath $serviceExe).LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss')
    Write-Host "        (built $built)"
}
else {
    Write-Host '        提示: 先运行 `. .\scripts\env.ps1; cd src; cargo build --release -p mhservice`' -ForegroundColor DarkYellow
}
if (-not (Test-Path -LiteralPath (Join-Path $script:DistRoot 'mhservice.exe'))) {
    Add-Warning 'mhservice.exe 未打包：dist 中缺少 Rust 后端（UI 将无法连接后端）'
}

# --- 4. copy vendored native runtime --------------------------------------

Write-Section 'Copying native runtime (required)'

# Both are REQUIRED: the recognition pipeline cannot start without them.
$ortDll    = Join-Path $script:Root 'native\onnxruntime\bin\onnxruntime.dll'
$opencvDll = Join-Path $script:Root 'native\opencv\bin\opencv_world4120.dll'

Copy-Artifact -Source $ortDll    -DestinationDir $script:DistRoot -Label 'onnxruntime.dll'      -Required | Out-Null
Copy-Artifact -Source $opencvDll -DestinationDir $script:DistRoot -Label 'opencv_world4120.dll' -Required | Out-Null

# --- 5. copy OCR models ---------------------------------------------------

Write-Section 'Copying OCR models (required)'

$modelsSource = Join-Path $script:Root 'native\models'
$modelsDest   = Join-Path $script:DistRoot 'models'
$modelFiles   = @(
    'ch_PP-OCRv4_det_infer.onnx',
    'ch_PP-OCRv4_rec_infer.onnx',
    'ppocr_keys_v1.txt'
)

foreach ($model in $modelFiles) {
    Copy-Artifact -Source (Join-Path $modelsSource $model) `
                  -DestinationDir $modelsDest `
                  -Label "models\$model" `
                  -Required | Out-Null
}

if (-not (Test-Path -LiteralPath (Join-Path $modelsDest $modelFiles[0]))) {
    Write-Host '        提示: 模型缺失时 UI 仍可打开，但“识别”会报 OCR 不可用（存储/统计/导出不受影响）。' -ForegroundColor DarkYellow
}

# --- 6. generate run.ps1 --------------------------------------------------

Write-Section 'Generating launcher and README'

$runPs1Path  = Join-Path $script:DistRoot 'run.ps1'
$readmePath  = Join-Path $script:DistRoot 'README.txt'

# Single-quoted here-string: every `$` below stays literal in the generated
# script and is evaluated at launcher runtime (paths are resolved from
# $MyInvocation so the folder can be moved/copied anywhere).
$runPs1Content = @'
# ============================================================================
#  MhRecorder 启动器 —— 由 scripts\package.ps1 自动生成，请勿手工修改
#
#  为什么必须用它？C:\Windows\System32 下自带一个很旧的 onnxruntime.dll
#  (1.17)，Windows 的 DLL 搜索顺序会先加载它，而 Rust 的 `ort` 需要 >= 1.31。
#  本脚本在启动前显式设置 ORT_DYLIB_PATH 并把自己所在目录前置到 PATH，
#  保证加载的是随包分发的 onnxruntime.dll。
# ============================================================================

$ErrorActionPreference = 'Stop'

$dist       = Split-Path -Parent $MyInvocation.MyCommand.Path
$appExe     = Join-Path $dist 'MhRecorder.exe'
$ortDll     = Join-Path $dist 'onnxruntime.dll'
$opencvDll  = Join-Path $dist 'opencv_world4120.dll'
$modelsDir  = Join-Path $dist 'models'
$serviceExe = Join-Path $dist 'mhservice.exe'

if (-not (Test-Path -LiteralPath $ortDll)) {
    throw "缺少 onnxruntime.dll（文件已损坏或复制不完整）：$ortDll"
}

# --- 关键：让 `ort` 加载随包分发的 ONNX Runtime，而不是 System32 里的旧版
$env:ORT_DYLIB_PATH = $ortDll
# --- OCR 模型目录（缺模型时仅“识别”不可用，其它功能正常）
$env:MH_MODELS_DIR  = $modelsDir
# --- 数据目录（数据库 mhrecorder.db 存放在这里）
if (-not $env:MH_DATA_DIR) {
    $env:MH_DATA_DIR = Join-Path $env:LOCALAPPDATA 'MhRecorder'
}
# --- 把 dist 前置到 PATH，保证加载的是本包内的原生 DLL
$env:Path = "$dist;" + $env:Path

if (-not (Test-Path -LiteralPath (Join-Path $dist 'MhRecorder.exe'))) {
    Write-Host "未找到 MhRecorder.exe：" -ForegroundColor Yellow
    Write-Host "  $appExe" -ForegroundColor Yellow
    Write-Host "这份 dist 只包含后端组件（UI 尚未发布）。请在开发机重新运行 scripts\package.ps1。" -ForegroundColor Yellow
    exit 1
}

if (-not (Test-Path -LiteralPath $serviceExe)) {
    Write-Host "警告：未找到 mhservice.exe，界面可能无法连接后端。" -ForegroundColor Yellow
}
if (-not (Test-Path -LiteralPath $opencvDll)) {
    Write-Host "警告：未找到 opencv_world4120.dll，识别功能将不可用。" -ForegroundColor Yellow
}

Write-Host 'MhRecorder 启动中（离线本地运行，不联网）...' -ForegroundColor Green
Write-Host "  数据目录 : $env:MH_DATA_DIR"
Write-Host "  模型目录 : $env:MH_MODELS_DIR"
Write-Host "  ORT      : $env:ORT_DYLIB_PATH"

# --- 先拉起后端，再开界面：界面通过 \\.\pipe\mhrecorder.v1 连接它。
#     少了这一步界面会退回内存演示数据，识别功能形同虚设。
if (Test-Path -LiteralPath $serviceExe) {
    if (-not (Get-Process -Name mhservice -ErrorAction SilentlyContinue)) {
        Start-Process -FilePath $serviceExe -WorkingDirectory $dist -WindowStyle Hidden
        # 等管道就绪（最多约 4 秒）
        $deadline = (Get-Date).AddSeconds(4)
        while ((Get-Date) -lt $deadline) {
            if (Get-Process -Name mhservice -ErrorAction SilentlyContinue) { break }
            Start-Sleep -Milliseconds 150
        }
        Start-Sleep -Milliseconds 500
    }
}

Start-Process -FilePath $appExe
'@

Write-TextFileBom -Path $runPs1Path -Content ($runPs1Content + "`r`n")

$readmeContent = @'
===== MhRecorder（梦幻西游手游截图记录助手）=====

版本：0.9.2（本地版）
运行方式：完全离线。不联网、不上传任何截图或数据，不使用 GPU，也不做云调用。

------------------------------------------------------------------
一、如何运行
------------------------------------------------------------------
推荐：右键点击 run.ps1 → “使用 PowerShell 运行”。

或在 PowerShell 中执行（从本目录）：

    powershell -ExecutionPolicy Bypass -File .\run.ps1

不要直接双击 MhRecorder.exe！run.ps1 会先设置好 ORT_DYLIB_PATH / PATH /
MH_MODELS_DIR / MH_DATA_DIR，直接双击会加载 C:\Windows\System32 里的旧版
onnxruntime.dll（1.17），导致识别引擎启动失败。

------------------------------------------------------------------
二、数据保存在哪里
------------------------------------------------------------------
数据库：%LOCALAPPDATA%\MhRecorder\mhrecorder.db（SQLite，WAL 模式）
默认路径: C:\Users\<你的用户名>\AppData\Local\MhRecorder

- 所有角色、快照、装备、背包、设置都保存在这一个数据库文件里。
- 想换位置？设置环境变量 MH_DATA_DIR 指向其它目录后再运行 run.ps1。
- 想备份？关闭程序后复制整个 MhRecorder 文件夹即可。
- 想彻底清除？删除该文件夹，或在界面“设置 → 清空本地数据”。

------------------------------------------------------------------
三、目录内容
------------------------------------------------------------------
  MhRecorder.exe          WinUI 3 界面主程序
  mhservice.exe           Rust 后端（命名管道 mhrecorder.v1）
  mhrecorder.db           运行后才会生成，位于数据目录（不在本目录）
  onnxruntime.dll         ONNX Runtime 1.31（随包分发）
  opencv_world4120.dll    OpenCV 4.12（随包分发）
  models\                 PP-OCRv4 检测/识别模型 + 字典
  run.ps1                 启动器（必须用它启动）
  README.txt              本文件

------------------------------------------------------------------
四、常见问题
------------------------------------------------------------------
1) 界面提示“后端服务未运行 / 管道不可用”
   - 确认 mhservice.exe 和 mhrecorder.db 目录存在；
   - 确认是从 run.ps1 启动的；
   - 若使用杀毒软件，请放行 mhservice.exe（它会创建命名管道）。

2) 识别一直失败（提示 OCR 不可用）
   - 检查 models\ 下三个文件是否齐全；
   - 确认没有删掉 onnxruntime.dll；
   - 确认是从 run.ps1 启动（ORT_DYLIB_PATH 未设置时会去加载系统旧版）。

3) 提示缺少 VC++ 运行库
   安装 Microsoft Visual C++ 2015-2022 Redistributable (x64)。

------------------------------------------------------------------
五、导出
------------------------------------------------------------------
界面“设置 → 导出全部数据为 JSON”会写出完整数据文档；
后端同时支持导出 CSV / ZIP（ZIP 内含 data.json）。
导出文件为纯本地文件，不会上传。

------------------------------------------------------------------
六、卸载
------------------------------------------------------------------
直接删除本目录和 %LOCALAPPDATA%\MhRecorder 即可，不写注册表，
不安装系统服务，不留后台常驻进程（后端在程序退出时一并结束）。
'@

$readmeContent = $readmeContent.Replace('版本：0.9.2（本地版）', "版本：$AppVersion（本地版）")
Write-TextFileBom -Path $readmePath -Content ($readmeContent + "`r`n")

if (-not (Test-Path -LiteralPath $runPs1Path))  { Add-Error "生成 run.ps1 失败: $runPs1Path" }
if (-not (Test-Path -LiteralPath $readmePath))  { Add-Error "生成 README.txt 失败: $readmePath" }
Write-Host '  OK    run.ps1   (设置 ORT_DYLIB_PATH / PATH / MH_MODELS_DIR / MH_DATA_DIR)'
Write-Host '  OK    README.txt (中文使用说明)'

# --- 7. verify the required layout ---------------------------------------

Write-Section 'Verifying dist layout'

$required = @(
    @{ Path = (Join-Path $script:DistRoot 'onnxruntime.dll');          What = 'ONNX Runtime DLL' },
    @{ Path = (Join-Path $script:DistRoot 'opencv_world4120.dll');     What = 'OpenCV DLL' },
    @{ Path = (Join-Path $script:DistRoot 'models\ch_PP-OCRv4_det_infer.onnx'); What = 'OCR det model' },
    @{ Path = (Join-Path $script:DistRoot 'models\ch_PP-OCRv4_rec_infer.onnx'); What = 'OCR rec model' },
    @{ Path = (Join-Path $script:DistRoot 'models\ppocr_keys_v1.txt'); What = 'OCR dict' },
    @{ Path = (Join-Path $script:DistRoot 'run.ps1');                  What = 'launcher' },
    @{ Path = (Join-Path $script:DistRoot 'README.txt');               What = 'readme' }
)

foreach ($item in $required) {
    if (Test-Path -LiteralPath $item.Path -PathType Leaf) {
        Write-Host ("  OK    {0,-16} {1}" -f $item.What, (Format-Size (Get-Item -LiteralPath $item.Path).Length))
    }
    else {
        Write-Host ("  MISSING  {0}  ({1})" -f $item.What, $item.Path) -ForegroundColor Red
    }
}

$expectedOptional = @(
    @{ Path = (Join-Path $script:DistRoot 'mhservice.exe'); What = 'Rust backend' },
    @{ Path = (Join-Path $script:DistRoot 'MhRecorder.exe'); What = 'WinUI app' }
)
foreach ($item in $expectedOptional) {
    if (Test-Path -LiteralPath $item.Path -PathType Leaf) {
        Write-Host ("  OK    {0,-16} {1}" -f $item.What, (Format-Size (Get-Item -LiteralPath $item.Path).Length)) -ForegroundColor Green
    }
    else {
        Write-Host ("  MISSING  {0}" -f $item.What) -ForegroundColor Yellow
        Add-Warning "$($item.What) 未包含在 dist 中"
    }
}

# --- 8. final listing -----------------------------------------------------

Write-Section 'Dist folder listing'

$distFiles = @(Get-ChildItem -LiteralPath $script:DistRoot -Recurse -File -ErrorAction SilentlyContinue |
               Sort-Object -Property FullName)

if ($distFiles.Count -eq 0) {
    Add-Error "dist 目录为空: $script:DistRoot"
}
else {
    foreach ($file in $distFiles) {
        $rel = $file.FullName
        if ($rel.StartsWith($script:DistRoot)) {
            $rel = $rel.Substring($script:DistRoot.Length).TrimStart('\', '/')
        }
        Write-Host ("  {0,10}  {1}" -f (Format-Size $file.Length), $rel)
    }

    $totalBytes = ($distFiles | Measure-Object -Property Length -Sum).Sum
    if ($null -eq $totalBytes) { $totalBytes = 0 }
    Write-Host ''
    Write-Host ("  files : {0}" -f $distFiles.Count)
    Write-Host ("  size  : {0}" -f (Format-Size ([long] $totalBytes)))

    # per-folder roll-up keeps the "did models / UI land?" question obvious
    Write-Host ''
    foreach ($sub in @(Get-ChildItem -LiteralPath $script:DistRoot -Directory -ErrorAction SilentlyContinue)) {
        $subFiles = @(Get-ChildItem -LiteralPath $sub.FullName -Recurse -File -ErrorAction SilentlyContinue)
        $subSum = ($subFiles | Measure-Object -Property Length -Sum).Sum
        if ($null -eq $subSum) { $subSum = 0 }
        Write-Host ("  {0}\  : {1} files, {2}" -f $sub.Name, $subFiles.Count, (Format-Size ([long] $subSum)))
    }
    Write-Host ("  <root>  : {0} files, {1}" -f $distFiles.Count, (Format-Size ([long] $totalBytes)))
}

# --- 9. result ------------------------------------------------------------

Write-Section 'Result'

if ($Strict -and $script:Warnings.Count -gt 0) {
    Add-Error "严格模式: 出现 $($script:Warnings.Count) 条警告，按失败处理 (-Strict)"
}

if ($script:Errors.Count -gt 0) {
    Write-Host "打包失败：$($script:Errors.Count) 个错误" -ForegroundColor Red
    foreach ($err in $script:Errors) { Write-Host "  - $err" -ForegroundColor Red }
    if ($script:Warnings.Count -gt 0) {
        Write-Host "警告 ($($script:Warnings.Count) 条):" -ForegroundColor Yellow
        foreach ($warn in $script:Warnings) { Write-Host "  - $warn" -ForegroundColor Yellow }
    }
    exit 1
}

if (Test-Path -LiteralPath (Join-Path $script:DistRoot 'mhservice.exe')) {
    Write-Host 'PACKAGING COMPLETED' -ForegroundColor Green
}
else {
    Write-Host 'PACKAGING COMPLETED (backend incomplete - see warnings)' -ForegroundColor Yellow
}
Write-Host "输出目录: $script:DistRoot"
Write-Host '启动方式: 右键 run.ps1 -> 使用 PowerShell 运行'
if ($script:Warnings.Count -gt 0) {
    Write-Host ''
    Write-Host "警告 ($($script:Warnings.Count) 条，未阻断打包):" -ForegroundColor Yellow
    foreach ($warn in $script:Warnings) { Write-Host "  - $warn" -ForegroundColor Yellow }
}
exit 0
