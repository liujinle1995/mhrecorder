#!/usr/bin/env pwsh
# ============================================================================
#  make_icon.ps1 — 生成 packaging\mhrecorder.ico（安装包与快捷方式用）
#
#  为什么用脚本画：项目里只有 UI 截图，没有品牌图标文件。手搓一个
#  "深蓝圆角方块 + MH" 比让安装向导显示 Inno 默认图标体面，也避免
#  "引用了不存在的 .ico 导致 ISCC 编译失败" 这类问题——图标由脚本保证存在。
#
#  用法： .\scripts\make_icon.ps1
# ============================================================================

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$outDir = Join-Path $root 'packaging'
if (-not (Test-Path -LiteralPath $outDir)) { New-Item -ItemType Directory -Force -Path $outDir | Out-Null }
$outPath = Join-Path $outDir 'mhrecorder.ico'

# 256x256 主图：Windows 会自己缩放到 16/24/32/48
$size = 256
$bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
$g.Clear([System.Drawing.Color]::Transparent)

# 圆角矩形路径
$radius = 52
$rect = New-Object System.Drawing.RectangleF(8, 8, ($size - 16), ($size - 16))
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$d = $radius * 2
$path.AddArc($rect.X, $rect.Y, $d, $d, 180, 90)
$path.AddArc(($rect.Right - $d), $rect.Y, $d, $d, 270, 90)
$path.AddArc(($rect.Right - $d), ($rect.Bottom - $d), $d, $d, 0, 90)
$path.AddArc($rect.X, ($rect.Bottom - $d), $d, $d, 90, 90)
$path.CloseFigure()

$brush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 31, 78, 140))
$g.FillPath($brush, $path)

# 左上角一道亮边，避免纯色平得像色块
$glossPath = New-Object System.Drawing.Drawing2D.GraphicsPath
$inner = $size - 16
$halfH = [int]($inner / 2)
$glossRect = New-Object System.Drawing.RectangleF(8, 8, $inner, $halfH)
$gd = $radius * 2
$glossPath.AddArc($glossRect.X, $glossRect.Y, $gd, $gd, 180, 90)
$glossPath.AddLine(($glossRect.Right - $radius), $glossRect.Y, ($glossRect.X + $radius), $glossRect.Y)
$glossPath.AddArc($glossRect.X, $glossRect.Y, $gd, $gd, 180, 90)
$glossPath.AddLine($glossRect.X, ($glossRect.Y + $radius), $glossRect.X, ($glossRect.Bottom))
$glossPath.AddLine($glossRect.X, $glossRect.Bottom, $glossRect.Right, $glossRect.Bottom)
$glossPath.AddLine($glossRect.Right, $glossRect.Bottom, $glossRect.Right, ($glossRect.Y + $radius))
$glossPath.CloseFigure()
$gloss = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(28, 255, 255, 255))
$g.FillPath($gloss, $glossPath)

# 白色 MH
$fontSize = 96
$font = New-Object System.Drawing.Font('Segoe UI', $fontSize, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
$fmt = New-Object System.Drawing.StringFormat
$fmt.Alignment = [System.Drawing.StringAlignment]::Center
$fmt.LineAlignment = [System.Drawing.StringAlignment]::Center
$textRect = New-Object System.Drawing.RectangleF(0, -4, $size, $size)
$white = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
$g.DrawString('MH', $font, $white, $textRect, $fmt)

$g.Dispose()

# 手写 ICO 容器 + PNG 负载（Vista+ 支持）。
# 用 Icon.FromHandle($bmp.GetHicon()) 保存会掉色：HICON 走的是旧设备相关的
# 调色板路径，蓝色会被渲染成青绿。直接按 PNG 塞进 ICONDIR 才保真。
$ms = New-Object System.IO.MemoryStream
$bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
$png = $ms.ToArray()
$ms.Dispose()
$bmp.Dispose()

$fs = [System.IO.File]::Create($outPath)
$bw = New-Object System.IO.BinaryWriter($fs)
$bw.Write([UInt16]0)            # reserved
$bw.Write([UInt16]1)            # type = icon
$bw.Write([UInt16]1)            # image count
$bw.Write([Byte]0)              # width  0 => 256
$bw.Write([Byte]0)              # height 0 => 256
$bw.Write([Byte]0)              # palette size
$bw.Write([Byte]0)              # reserved
$bw.Write([UInt16]1)            # color planes
$bw.Write([UInt16]32)           # bits per pixel
$bw.Write([UInt32]$png.Length)  # payload size
$bw.Write([UInt32]22)           # payload offset (6 + 16)
$bw.Write($png)
$bw.Flush()
$bw.Dispose()
$fs.Close()

Write-Host "生成 $outPath  ($([math]::Round((Get-Item $outPath).Length / 1KB, 1)) KB)" -ForegroundColor Green
