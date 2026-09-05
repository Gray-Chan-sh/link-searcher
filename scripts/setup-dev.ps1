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
    [switch]$IncludeTesseract,
    [switch]$ForceRedownload
)

$ErrorActionPreference = "Stop"
$ROOT = Split-Path -Parent $PSScriptRoot

# Self-elevate: VS Build Tools 安装需要管理员权限。
# 非提权进程启动 setup.exe 时，UAC fork 导致 PowerShell 的 & 调用不阻塞，
# setup.exe 返回 0 但实际安装仍在后台进行，复检 vswhere 误报失败。
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host "==> 需要管理员权限（VS Build Tools 安装），正在提权重跑..." -ForegroundColor Yellow
    $relaunchArgs = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $PSCommandPath)
    if ($SkipSystemDeps) { $relaunchArgs += "-SkipSystemDeps" }
    if ($IncludeTesseract) { $relaunchArgs += "-IncludeTesseract" }
    if ($ForceRedownload) { $relaunchArgs += "-ForceRedownload" }
    try {
        $proc = Start-Process powershell -Verb RunAs -ArgumentList $relaunchArgs -Wait -PassThru
        exit $proc.ExitCode
    } catch {
        Write-Host "!! 用户取消了提权。请以管理员身份手动运行：" -ForegroundColor Red
        Write-Host "    右键 PowerShell → 以管理员身份运行 → .\scripts\setup-dev.ps1"
        Read-Host "按 Enter 退出"
        exit 1
    }
}

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
$dlUrl = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v${ver}/$(Split-Path $archive -Leaf)"

# 验证 tar.bz2 可读性（tar.exe Windows 10 1803+ 自带）
function Test-ArchiveIntact([string]$path) {
    if (-not (Test-Path $path)) { return $false }
    $prevEAP = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & tar -tf $path 2>&1 | Out-Null
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prevEAP
    return ($code -eq 0)
}

$needDownload = $false
if ($ForceRedownload) {
    if (Test-Path $archive) {
        Write-Host "    -ForceRedownload：删除现有压缩包重新下载..." -ForegroundColor Yellow
        Remove-Item $archive -Force
    }
    $needDownload = $true
} elseif (-not (Test-Path $archive)) {
    $needDownload = $true
} elseif (-not (Test-ArchiveIntact $archive)) {
    Write-Host "    sherpa-onnx 压缩包损坏（tar -tf 验证失败），删除重新下载..." -ForegroundColor Yellow
    Remove-Item $archive -Force
    $needDownload = $true
} else {
    Write-Host "    sherpa-onnx archive already present (verified)"
}

if ($needDownload) {
    Write-Host "==> 下载 sherpa-onnx Windows 静态库（约 18MB，一次性）..." -ForegroundColor Yellow
    Write-Host "    URL: $dlUrl"
    Write-Host "    如果下载失败，请用浏览器/加速器下载后放到:"
    Write-Host "    $archive"
    try {
        # --ssl-no-revoke：国内网络/代理下 schannel 证书吊销检查常失败
        # （CRYPT_E_NO_REVOCATION_CHECK），此处绕过吊销检查。
        curl.exe -L --fail --retry 3 --ssl-no-revoke -o $archive $dlUrl
        $curlExit = $LASTEXITCODE
        if ($curlExit -ne 0 -or -not (Test-Path $archive)) {
            throw "curl exit=$curlExit"
        }
        # 下载后验证完整性
        if (-not (Test-ArchiveIntact $archive)) {
            Remove-Item $archive -Force
            throw "下载的文件验证失败（tar -tf 失败）"
        }
        Write-Host "    下载完成，验证通过。"
    } catch {
        Write-Warning "自动下载失败（$_）。请用浏览器/加速器下载后放到："
        Write-Warning "  $archive"
        Write-Warning "然后重跑本脚本。"
    }
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

# 检测 VS / Build Tools 是否装了 VC++ 工具链。cargo/cc-rs 通过 vswhere 自动
# 定位 MSVC，不要求 cl 在 PATH 中，所以不能用 Get-Command cl 判断。
function Test-MsvcPresent {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) { return $false }
    $prevEAP = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $path = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
    $ErrorActionPreference = $prevEAP
    return [bool]($path | Where-Object { $_ -and $_.Trim() })
}

# 用 vswhere 找出一个已存在的 VS 实例安装路径（可能已装但缺 C++ 组件）。
function Get-VsInstallPath {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) { return $null }
    $prevEAP = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $path = & $vswhere -latest -products * -property installationPath 2>$null
    $ErrorActionPreference = $prevEAP
    if ($path) { return ($path | Where-Object { $_ -and $_.Trim() } | Select-Object -First 1) }
    return $null
}

# 直接调用 VS 官方安装器补装/安装 C++ 组件。
# 为什么不走 winget：winget 对"已安装"的 BuildTools 会走"找升级"逻辑，
# 把 --override 的 --add 组件参数吞掉或转成无操作，实测 --force 也无法让
# winget 把组件加上。直接进入官方 installer 的 modify/install 通道最可靠。
$setupExe = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\setup.exe"
$setupExists = Test-Path $setupExe

# 安装所需的组件参数（每个 token 必须是独立 argv，不能拼成一个字符串）。
# --noUpdateInstaller：跳过 VS Installer 自更新（自更新会因 temp 文件缺失卡死）。
#   setup.exe 是 bootstrapper，exit 0 不代表安装完成——下方用重试循环等真正装完。
$vsArgs = @("--add", "Microsoft.VisualStudio.Workload.VCTools", "--add", "Microsoft.VisualStudio.Component.Windows11SDK.26100", "--noUpdateInstaller")

if (-not (Test-MsvcPresent)) {
    Write-Host "==> 未检测到可用的 MSVC 工具链（link.exe 依赖它）。" -ForegroundColor Yellow

    if (-not $setupExists) {
        Write-Host "!! 未检测到 VS/BuildTools，且未找到官方 installer（默认安装路径之外）。" -ForegroundColor Yellow
        Write-Host "    请从 https://visualstudio.microsoft.com/downloads/ 下载 Build Tools，"
        Write-Host "    安装时勾选「使用 C++ 的桌面开发」工作负载。"
    } else {
        Write-Host "    即将启动官方 VS 安装器（进度窗口装完会自动关闭）..." -ForegroundColor Yellow
        $installedPath = Get-VsInstallPath
        if ($installedPath) {
            # BuildTools/VS 已存在但缺 C++ 组件 → modify 已有装加组件
            Write-Host "    检测到 VS 已存在（$installedPath），缺 C++ 组件 —— 直接 modify 补装..."
        } else {
            Write-Host "    未找到现有 VS —— 首次安装（含 C++ 桌面工作负载）..."
        }

        $prevEAP = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        if ($installedPath) {
            & $setupExe modify --installPath $installedPath --passive --norestart @vsArgs
        } else {
            & $setupExe install --channelUri https://aka.ms/vs/17/release/channel --productId Microsoft.VisualStudio.Product.BuildTools --passive --norestart @vsArgs
        }
        $exit = $LASTEXITCODE
        $ErrorActionPreference = $prevEAP

        if ($exit -ne 0) {
            Write-Host "!! 安装器返回失败 exit=$exit。请打开 VS Installer 手动补装「使用 C++ 的桌面开发」工作负载。" -ForegroundColor Yellow
        }

        # 复检：setup.exe 是 bootstrapper，exit 0 后安装可能仍在后台进行。
        # 重试循环最多 6 次 × 10s = 60s，等 vswhere 元数据刷新。
        $msvcReady = $false
        for ($i = 0; $i -lt 6; $i++) {
            if ($i -gt 0) {
                Write-Host "    等待安装完成...（$($i * 10)s）" -ForegroundColor Yellow
                Start-Sleep -Seconds 10
            }
            if (Test-MsvcPresent) {
                $msvcReady = $true
                break
            }
        }
        if ($msvcReady) {
            Write-Host "    MSVC 工具链就绪。"
        } else {
            Write-Host "!! 复检仍未检测到 MSVC 工具链。请打开 VS Installer 手动补装「使用 C++ 的桌面开发」工作负载。" -ForegroundColor Yellow
        }
    }
} else {
    Write-Host "    MSVC 工具链已就绪（vswhere 检测通过）。"
}

Write-Host ""
Write-Host "==> 完成。之后：" -ForegroundColor Green
Write-Host "    npm ci"
Write-Host "    cd src-tauri"
Write-Host "    cargo build"
Write-Host ""
Write-Host "    注意：winget 装完的 poppler/ffmpeg/tesseract 需要重新打开终端才在 PATH 中；"
Write-Host "    若 `cargo build` 报找不到，请开新终端再试。"
Write-Host ""
Write-Host "按 Enter 退出..." -ForegroundColor Cyan
Read-Host
