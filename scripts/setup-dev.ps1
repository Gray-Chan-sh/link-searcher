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
        # --ssl-no-revoke：国内网络/代理下 schannel 证书吊销检查常失败
        # （CRYPT_E_NO_REVOCATION_CHECK），此处绕过吊销检查。
        curl.exe -L --fail --retry 3 --ssl-no-revoke -o $archive `
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/v${ver}/$(Split-Path $archive -Leaf)"
        $curlExit = $LASTEXITCODE
        if ($curlExit -ne 0 -or -not (Test-Path $archive)) {
            throw "curl exit=$curlExit"
        }
    } catch {
        Write-Warning "自动下载失败（$_）。请用浏览器/加速器下载后放到："
        Write-Warning "  $archive"
        Write-Warning "然后重跑本脚本（已存在的文件会跳过下载）。"
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

# 5. 工具链检查与安装
Write-Host ""

# winget 已装检查 helper：`winget list --id X` 退出码为 0（找到包）即 $true。
# 需要临时放宽 $ErrorActionPreference：winget list 找不到包时退出码非 0，
# 在 "Stop" 下会被当作终止错误。
function Test-WingetPackageInstalled([string]$id) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) { return $false }
    $prevEAP = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    winget list --id $id --accept-source-agreements 2>&1 | Out-Null
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prevEAP
    return ($code -eq 0)
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    if (Test-WingetPackageInstalled "Rustlang.Rustup") {
        Write-Host "!! 检测到 Rust 已通过 winget 安装，但 cargo 不在当前 PATH。" -ForegroundColor Yellow
        Write-Host "    请关闭本终端后重新打开（或注销/重启一次）再跑 cargo build。"
        Write-Host "    若重开后仍找不到 cargo，先执行一次：rustup default stable"
    } elseif (Get-Command winget -ErrorAction SilentlyContinue) {
        Write-Host "==> winget 安装 Rust（Rustlang.Rustup，免管理员）..." -ForegroundColor Yellow
        $prevEAP = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        winget install -e --id Rustlang.Rustup --silent --accept-package-agreements --accept-source-agreements
        $exit = $LASTEXITCODE
        $ErrorActionPreference = $prevEAP
        # 0 = installed; 0x8A15002B (-1978335189) = already at newest version.
        if ($exit -eq 0 -or $exit -eq -1978335189) {
            Write-Host "    Rust 已就绪。请重开终端后先跑一次：rustup default stable"
            Write-Host '    （国内加速可选：$env:RUSTUP_DIST_SERVER="https://rsproxy.cn" 后 rustup-init）'
        } else {
            Write-Host "!! winget 安装 Rust 失败（exit=$exit），请手动执行：" -ForegroundColor Yellow
            Write-Host '   $env:RUSTUP_DIST_SERVER="https://rsproxy.cn"; $env:RUSTUP_UPDATE_ROOT="https://rsproxy.cn/rustup"; irm https://rsproxy.cn/rustup-init.exe | iex'
        }
    } else {
        Write-Host "!! Rust 未安装且无 winget。请用国内源手动安装：" -ForegroundColor Yellow
        Write-Host '   $env:RUSTUP_DIST_SERVER="https://rsproxy.cn"; $env:RUSTUP_UPDATE_ROOT="https://rsproxy.cn/rustup"; irm https://rsproxy.cn/rustup-init.exe | iex'
    }
}
if (-not (Get-Command node -ErrorAction SilentlyContinue)) {
    Write-Host "!! Node.js 未安装（建议 >=20）：https://nodejs.org 或 winget install OpenJS.NodeJS.LTS"
}
if (-not (Get-Command cl -ErrorAction SilentlyContinue)) {
    Write-Host "!! 未检测到 MSVC 编译器（cargo 构建必需）。两种方式（任选）：" -ForegroundColor Yellow
    Write-Host "    A. Visual Studio Build Tools（含 C++ 桌面工作负载，体积大需管理员）："
    Write-Host '       winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.Windows11SDK.26100"'
    Write-Host "    B. 仅安装 MSVC 命令行工具链（更轻量，仍需管理员）："
    Write-Host "       winget install Microsoft.VisualStudio.2022.BuildTools"
    Write-Host "    装完重开终端后 cl 即可用；本脚本不自动安装（体积大且需 UAC）。"
}

Write-Host ""
Write-Host "==> 完成。之后：" -ForegroundColor Green
Write-Host "    npm ci"
Write-Host "    cd src-tauri"
Write-Host "    cargo build"
Write-Host ""
Write-Host "    注意：winget 装完的 poppler/ffmpeg/tesseract 需要重新打开终端才在 PATH 中；"
Write-Host "    若 `cargo build` 报找不到，请开新终端再试。"
