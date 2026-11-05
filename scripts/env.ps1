# Rust backend build environment.
# Source this (`. .\scripts\env.ps1`) before running cargo in a bare shell.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

# LLVM (clang + libclang) is needed by opencv-binding-generator, not by our own
# code. It moves around between machines and installers, so look in the two
# places it actually lives rather than failing with "Can't find clang binary"
# halfway through a build.
$llvmBin = "C:\Program Files\LLVM\bin"
if (-not (Test-Path (Join-Path $llvmBin 'clang.exe'))) {
    $scoopLlvm = Join-Path $env:USERPROFILE 'scoop\apps\llvm\current\bin'
    if (Test-Path (Join-Path $scoopLlvm 'clang.exe')) { $llvmBin = $scoopLlvm }
}
if (-not (Test-Path (Join-Path $llvmBin 'clang.exe'))) {
    Write-Warning "clang.exe not found in C:\Program Files\LLVM\bin or scoop; opencv will fail to build. Run: scoop install llvm"
}

$env:LIBCLANG_PATH = $llvmBin
$env:OPENCV_INCLUDE_PATHS = "$root\native\opencv\include"
$env:OPENCV_LINK_PATHS = "$root\native\opencv\lib"
$env:OPENCV_LINK_LIBS = "opencv_world4120"
$env:OPENCV_MSVC_RUNTIME = "dynamic"

# CRITICAL: C:\Windows\system32 ships an onnxruntime.dll v1.17 which would be
# picked up before ours and then rejected by `ort` (which needs >= 1.31).
# Point `ort` at the vendored 1.31.1 build explicitly.
$env:ORT_DYLIB_PATH = "$root\native\onnxruntime\bin\onnxruntime.dll"

# ONNX Runtime is loaded dynamically at process start (ort `load-dynamic`),
# so the DLL must be discoverable, and LLVM's clang must precede anything else
# on PATH for opencv's binding generator.
$env:Path = "$llvmBin;$root\native\opencv\bin;$root\native\onnxruntime\bin;$env:Path"

$env:MH_MODELS_DIR = "$root\native\models"

Write-Host "mhrecorder rust env ready" -ForegroundColor Green
Write-Host "  OPENCV_INCLUDE_PATHS = $env:OPENCV_INCLUDE_PATHS"
Write-Host "  OPENCV_LINK_PATHS    = $env:OPENCV_LINK_PATHS"
Write-Host "  MH_MODELS_DIR        = $env:MH_MODELS_DIR"
Write-Host "  ORT_DYLIB_PATH       = $env:ORT_DYLIB_PATH"
