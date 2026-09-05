# Link-Searcher 开发环境一键清理（Windows PowerShell）
#
# 清理 setup-dev.ps1 产生的构建产物，让源码回到初始状态（保留系统级
# 配置如 cargo/npm 镜像、lld-link、winget 装的 poppler/ffmpeg/LLVM/Rust，
# 它们可能被其它项目共用，不在此清理范围）。
#
# 清理内容：
#   - src-tauri\target\              Rust 编译产物（含解压的 sherpa-onnx）
#   - node_modules\                  npm ci 产物
#   - dist\ dist-ssr\                vite 构建产物
#   - third_party\sherpa-onnx\       sherpa-onnx 下载缓存
#
# 保留：~/.cargo/config.toml、项目 .npmrc、src-tauri/.cargo/config.toml、
#       SHERPA_ONNX_ARCHIVE_DIR 用户环境变量、数据目录模型（运行数据）。
#
# 参数：
#   -Yes  跳过确认直接删
#
# 幂等：可重复执行。

param(
    [switch]$Yes
)

$ErrorActionPreference = "Stop"
$ROOT = Split-Path -Parent $PSScriptRoot

Write-Host "==> Link-Searcher dev clean (root: $ROOT)" -ForegroundColor Cyan

$targets = @(
    @{ Path = Join-Path $ROOT "src-tauri\target";           Name = "Rust 编译产物" },
    @{ Path = Join-Path $ROOT "node_modules";               Name = "npm 依赖" },
    @{ Path = Join-Path $ROOT "dist";                       Name = "vite 产物" },
    @{ Path = Join-Path $ROOT "dist-ssr";                   Name = "vite SSR 产物" },
    @{ Path = Join-Path $ROOT "third_party\sherpa-onnx";   Name = "sherpa-onnx 下载缓存" }
)

$existing = @()
foreach ($t in $targets) {
    if (Test-Path $t.Path) {
        $existing += $t
    }
}

if ($existing.Count -eq 0) {
    Write-Host "    无需清理——构建产物不存在。" -ForegroundColor Green
    exit 0
}

Write-Host "    将删除以下构建产物（系统级配置与数据目录模型保留）："
foreach ($t in $existing) {
    Write-Host "    - $($t.Path)"
}

# 体积估算
$totalSize = 0
foreach ($t in $existing) {
    $size = (Get-ChildItem -Path $t.Path -Recurse -File -ErrorAction SilentlyContinue |
             Measure-Object -Property Length -Sum -ErrorAction SilentlyContinue).Sum
    if ($size) { $totalSize += $size }
}
if ($totalSize -gt 0) {
    $sizeMB = [math]::Round($totalSize / 1MB, 0)
    Write-Host "    预计释放: ${sizeMB} MB"
}

Write-Host "    清空数据目录下的模型（PaddleOCR/BGE/FunASR ~965MB）请手动删："
Write-Host "      Remove-Item -Recurse -Force `"$env:LOCALAPPDATA\link-searcher\models`""

if (-not $Yes) {
    $ans = Read-Host "确认删除？[y/N]"
    if ($ans -ne "y" -and $ans -ne "Y") {
        Write-Host "已取消"
        exit 0
    }
}

foreach ($t in $existing) {
    Remove-Item -Path $t.Path -Recurse -Force -ErrorAction SilentlyContinue
    if (-not (Test-Path $t.Path)) {
        Write-Host "    已删除: $($t.Path)" -ForegroundColor Green
    } else {
        Write-Host "    !! 删除失败（可能被占用）: $($t.Path)" -ForegroundColor Yellow
    }
}

Write-Host ""
Write-Host "==> 完成。之后重新就绪：" -ForegroundColor Green
Write-Host "    .\scripts\setup-dev.bat"
Write-Host "    npm ci"
Write-Host "    cd src-tauri"
Write-Host "    cargo build"