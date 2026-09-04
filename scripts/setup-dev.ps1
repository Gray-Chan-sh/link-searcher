# Link-Searcher 开发环境一键配置（Windows PowerShell）
#
# 用途：clone 源码后在 Windows 开发机快速就绪。
#   - 配置 cargo 国内镜像（rsproxy.cn）
#   - 配置 npm 镜像（npmmirror.com）
#   - 让 git 依赖（tauri-plugin-mcp 等）复用系统 git 代理/凭证
#   - 下载 sherpa-onnx 预编译静态库，避免 cargo build 时从 GitHub 拉取失败
#   - 自动安装可选系统依赖：poppler（扫描版 PDF 渲染）、ffmpeg（音频解码）、
#     tesseract（可选 OCR CLI）——通过 winget，已装则跳过
#   - 提示安装 VS Build Tools / WebView2
#
# 参数：
#   -SkipSystemDeps   不自动安装 poppler/ffmpeg/tesseract（仅提示）
#   -IncludeTesseract 额外安装 tesseract OCR CLI（UB-Mannheim.TesseractOCR）
#
# 幂等：可重复执行。管理员权限不是必须（只写用户级配置与缓存）。

param(
    [switch]$SkipSystemDeps,
    [switch]$IncludeTesseract
)

$ErrorActionPreference = "Stop"
$ROOT = Split-Path -Parent $PSScriptRoot

Write-Host "==> Link-Searcher dev setup (root: $ROOT)" -ForegroundColor Cyan

# 1. Cargo 镜像（rsproxy.cn）
$cargoDir = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME ".cargo" }
New-Item -ItemType Directory -Force -Path $cargoDir | Out-Null
$config = Join-Path $cargoDir "config.toml"
$cargoBlock = @"

# Added by link-searcher scripts/setup-dev.ps1
[source.crates-io]
replace-with = "rsproxy-sparse"

[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"

[registries.rsproxy]
index = "sparse+https://rsproxy.cn/index/"

[net]
git-fetch-with-cli = true
"@
if (Test-Path $config) {
    $content = Get-Content $config -Raw -ErrorAction SilentlyContinue
    if ($content -notmatch "rsproxy") {
        Add-Content -Path $config -Value $cargoBlock
        Write-Host "    cargo mirror appended -> $config"
    } else {
        Write-Host "    cargo mirror already configured"
    }
} else {
    Set-Content -Path $config -Value $cargoBlock
    Write-Host "    cargo mirror -> $config"
}

# 2. npm 镜像（项目级 .npmrc）
$npmrc = Join-Path $ROOT ".npmrc"
if (-not (Test-Path $npmrc)) {
    Set-Content -Path $npmrc -Value "registry=https://registry.npmmirror.com"
    Write-Host "    npm mirror -> $npmrc"
} else {
    Write-Host "    .npmrc already present"
}

# 3. sherpa-onnx 预编译库（构建期从 GitHub 拉，国内易失败）
#    sherpa-onnx-sys 的 build.rs 会读 SHERPA_ONNX_ARCHIVE_DIR，找不到才联网。
$ver = "1.13.4"
$dlDir = Join-Path $ROOT "third_party\sherpa-onnx"
New-Item -ItemType Directory -Force -Path $dlDir | Out-Null
$archive = Join-Path $dlDir "sherpa-onnx-v${ver}-win-x64-static-MT-Release-lib.tar.bz2"
if (-not (Test-Path $archive)) {
    Write-Host "==> 下载 sherpa-onnx Windows 静态库（约 18MB，一次性）..." -ForegroundColor Yellow
    Write-Host "    URL: https://github.com/k2-fsa/sherpa-onnx/releases/download/v${ver}/$(Split-Path $archive -Leaf)"
    Write-Host "    如果下载失败，请用浏览器/加速器下载后放到:"
    Write-Host "    $archive"
    try {
        curl.exe -L --fail --retry 3 -o $archive `
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/v${ver}/$(Split-Path $archive -Leaf)"
    } catch {
        Write-Warning "自动下载失败，请手动下载后重跑本脚本。"
    }
} else {
    Write-Host "    sherpa-onnx archive already present"
}
# 供 cargo build 使用（当前会话 + 持久化到用户环境变量）
$env:SHERPA_ONNX_ARCHIVE_DIR = $dlDir
[Environment]::SetEnvironmentVariable("SHERPA_ONNX_ARCHIVE_DIR", $dlDir, "User")
Write-Host "    SHERPA_ONNX_ARCHIVE_DIR=$dlDir (已写入用户环境变量)"

# 4. 可选系统依赖：自动检测缺失并 winget 安装
if (-not $SkipSystemDeps) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        Write-Warning "未检测到 winget（Windows 10 1709+ / 11 自带；也可从 Microsoft Store 安装 App Installer）。"
        Write-Warning "跳过自动安装，请手动安装："
        Write-Warning "  poppler:  winget install -e --id oschwartz10612.Poppler"
        Write-Warning "  ffmpeg:   winget install -e --id Gyan.FFmpeg"
        if ($IncludeTesseract) {
            Write-Warning "  tesseract: winget install -e --id UB-Mannheim.TesseractOCR"
        }
    } else {
        # 包 ID -> 检测命令 -> 说明
        $deps = @(
            @{ Id = "oschwartz10612.Poppler"; Probe = "pdftoppm"; Name = "poppler（扫描版 PDF 渲染）" },
            @{ Id = "Gyan.FFmpeg";            Probe = "ffmpeg";   Name = "ffmpeg（音频解码）" }
        )
        if ($IncludeTesseract) {
            $deps += @{ Id = "UB-Mannheim.TesseractOCR"; Probe = "tesseract"; Name = "tesseract（OCR CLI）" }
        }
        foreach ($d in $deps) {
            if (Get-Command $d.Probe -ErrorAction SilentlyContinue) {
                Write-Host "    $($d.Name): 已安装（$($d.Probe)）"
                continue
            }
            Write-Host "==> winget 安装 $($d.Name) ..." -ForegroundColor Yellow
            # 安装失败不应中断整个脚本（幂等可重跑）
            $prevEAP = $ErrorActionPreference
            $ErrorActionPreference = "Continue"
            winget install -e --id $d.Id --silent --accept-package-agreements --accept-source-agreements
            $exit = $LASTEXITCODE
            $ErrorActionPreference = $prevEAP
            if ($exit -eq 0) {
                Write-Host "    $($d.Name) 安装完成"
            } else {
                Write-Warning "$($d.Name) 安装失败（exit=$exit），请手动运行：winget install -e --id $($d.Id)"
            }
        }
    }
} else {
    Write-Host "==> 跳过系统依赖安装（-SkipSystemDeps）。如需安装："
    Write-Host "    winget install -e --id oschwartz10612.Poppler"
    Write-Host "    winget install -e --id Gyan.FFmpeg"
    if ($IncludeTesseract) {
        Write-Host "    winget install -e --id UB-Mannheim.TesseractOCR"
    }
}

# 5. 工具链检查
Write-Host ""
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "!! Rust 未安装。用国内源安装：" -ForegroundColor Yellow
    Write-Host '   $env:RUSTUP_DIST_SERVER="https://rsproxy.cn"; $env:RUSTUP_UPDATE_ROOT="https://rsproxy.cn/rustup"; irm https://rsproxy.cn/rustup-init.exe | iex'
}
if (-not (Get-Command node -ErrorAction SilentlyContinue)) {
    Write-Host "!! Node.js 未安装（建议 >=20）：https://nodejs.org 或 winget install OpenJS.NodeJS.LTS"
}
if (-not (Get-Command cl -ErrorAction SilentlyContinue)) {
    Write-Host "!! 未检测到 MSVC 编译器。请安装 Visual Studio Build Tools（含 MSVC + Windows SDK），"
    Write-Host "   或：winget install Microsoft.VisualStudio.2022.BuildTools"
}

Write-Host ""
Write-Host "==> 完成。之后：" -ForegroundColor Green
Write-Host "    npm ci"
Write-Host "    cd src-tauri"
Write-Host "    cargo build"
Write-Host ""
Write-Host "    注意：winget 装完的 poppler/ffmpeg/tesseract 需要重新打开终端才在 PATH 中；"
Write-Host "    若 `cargo build` 报找不到，请开新终端再试。"
