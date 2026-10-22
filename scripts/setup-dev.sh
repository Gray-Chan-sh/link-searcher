#!/usr/bin/env bash
# Link-Searcher 开发环境一键配置（macOS / Linux）
#
# 用途：clone 源码后在开发机快速就绪，无需手工配置镜像。
#   - 配置 cargo 国内镜像（rsproxy.cn，可改环境变量 CARGO_REGISTRY_MIRROR）
#   - 配置 npm 镜像（npmmirror.com，可改 NPM_REGISTRY_MIRROR）
#   - 让 git 依赖（tauri-plugin-mcp 等）复用系统 git 的代理/凭证
#   - 自动安装平台系统依赖：poppler（扫描版 PDF 渲染）、ffmpeg（音频解码）、
#     tesseract（可选 OCR CLI）——macOS 用 Homebrew、Debian/Ubuntu 用 apt，
#     已装则跳过
#   - 提示把模型发布资产下载到 dev 目录（可选）
#
# 参数：
#   --skip-system-deps  不自动安装 poppler/ffmpeg/tesseract（仅提示）
#   --include-tesseract 额外安装 tesseract OCR CLI
#
# 幂等：可重复执行。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_REGISTRY_MIRROR="${CARGO_REGISTRY_MIRROR:-rsproxy-sparse}"
NPM_REGISTRY_MIRROR="${NPM_REGISTRY_MIRROR:-https://registry.npmmirror.com}"
SKIP_SYSTEM_DEPS=0
INCLUDE_TESSERACT=0

for arg in "$@"; do
  case "$arg" in
    --skip-system-deps) SKIP_SYSTEM_DEPS=1 ;;
    --include-tesseract) INCLUDE_TESSERACT=1 ;;
    *) echo "未知参数: $arg（支持 --skip-system-deps / --include-tesseract）"; exit 1 ;;
  esac
done

echo "==> Link-Searcher dev setup (root: $ROOT)"

# 1. Cargo 镜像
CARGO_DIR="${CARGO_HOME:-$HOME/.cargo}"
mkdir -p "$CARGO_DIR"
CONFIG="$CARGO_DIR/config.toml"
if [ ! -f "$CONFIG" ] || ! grep -q "rsproxy" "$CONFIG" 2>/dev/null; then
  cat >> "$CONFIG" <<EOF

# Added by link-searcher scripts/setup-dev.sh
[source.crates-io]
replace-with = "$CARGO_REGISTRY_MIRROR"

[source.$CARGO_REGISTRY_MIRROR]
registry = "sparse+https://rsproxy.cn/index/"

[registries.rsproxy]
index = "sparse+https://rsproxy.cn/index/"

[net]
git-fetch-with-cli = true
EOF
  echo "    cargo mirror -> $CARGO_DIR/config.toml"
else
  echo "    cargo mirror already configured"
fi

# 2. npm 镜像（写入项目 .npmrc）
NPMRC="$ROOT/.npmrc"
if [ ! -f "$NPMRC" ] || ! grep -q "registry" "$NPMRC" 2>/dev/null; then
  echo "registry=$NPM_REGISTRY_MIRROR" > "$NPMRC"
  echo "    npm mirror -> $NPMRC"
else
  echo "    .npmrc already present"
fi

# 3. Rust toolchain
if ! command -v cargo >/dev/null; then
  echo "    !! cargo 未安装。请先安装 Rust："
  echo "       curl --proto '=https' --tlsv1.2 -sSf https://rsproxy.cn/rustup-init.sh | sh -s -- -y"
  echo "       （或设置 RUSTUP_DIST_SERVER=https://rsproxy.cn 后走官方 rustup）"
fi

# 4. Node
if ! command -v node >/dev/null; then
  echo "    !! Node.js 未安装（建议 >=20）。macOS: brew install node / 官网下载"
fi

# 5. 平台系统依赖：自动检测缺失并安装
#    brew / apt 需要交互式确认与 sudo，安装失败不中断（保持幂等可重跑）。
OS="$(uname -s)"
if [ "$SKIP_SYSTEM_DEPS" = "1" ]; then
  echo "==> 跳过系统依赖安装（--skip-system-deps）。所需命令："
  case "$OS" in
    Darwin) echo "    brew install poppler ffmpeg${INCLUDE_TESSERACT:+ tesseract tesseract-lang}" ;;
    Linux)  echo "    sudo apt install -y poppler-utils ffmpeg${INCLUDE_TESSERACT:+ tesseract-ocr} \\"
            echo "      build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev \\"
            echo "      libayatana-appindicator3-dev libssl-dev libsoup-3.0-dev libxdo-dev libgbm-dev \\"
            echo "      libpipewire-0.3-dev clang libclang-dev" ;;
  esac
elif [ "$OS" = "Darwin" ]; then
  if ! command -v brew >/dev/null; then
    echo "    !! 未检测到 Homebrew。无法自动安装，请手动："
    echo "       /bin/bash -c \"\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""
  else
    echo "==> 检查 Homebrew 依赖..."
    need=()
    command -v pdftoppm >/dev/null 2>&1 || need+=(poppler)
    command -v ffmpeg >/dev/null 2>&1 || need+=(ffmpeg)
    if [ "$INCLUDE_TESSERACT" = "1" ] && ! command -v tesseract >/dev/null 2>&1; then
      need+=(tesseract tesseract-lang)
    fi
    if [ "${#need[@]}" -gt 0 ]; then
      echo "==> brew install ${need[*]} ..."
      brew install "${need[@]}" || echo "    !! brew 安装失败，请手动运行上面的命令"
    else
      echo "    poppler/ffmpeg 已就绪（tesseract 未勾选或已安装）"
    fi
  fi
elif [ "$OS" = "Linux" ]; then
  # 仅对 Debian/Ubuntu 系自动尝试；其它发行版只提示
  if command -v apt-get >/dev/null 2>&1; then
    echo "==> 检查 apt 依赖（运行时 + Tauri 构建）..."
    need=()
    # --- 运行时 ---
    command -v pdftoppm >/dev/null 2>&1 || need+=(poppler-utils)
    command -v ffmpeg >/dev/null 2>&1 || need+=(ffmpeg)
    if [ "$INCLUDE_TESSERACT" = "1" ] && ! command -v tesseract >/dev/null 2>&1; then
      need+=(tesseract-ocr)
    fi
    # --- 构建：C 工具链 ---
    { command -v cc >/dev/null 2>&1 && command -v make >/dev/null 2>&1; } || need+=(build-essential)
    command -v pkg-config >/dev/null 2>&1 || need+=(pkg-config)
    # --- 构建：Tauri / GTK / WebKit 链接期（按 pkg-config 探测，缺什么装什么）---
    pkg_need() { pkg-config --exists "$1" 2>/dev/null || need+=("$2"); }
    pkg_need webkit2gtk-4.1 libwebkit2gtk-4.1-dev
    pkg_need gtk+-3.0 libgtk-3-dev
    pkg_need librsvg-2.0 librsvg2-dev
    pkg_need ayatana-appindicator3-0.1 libayatana-appindicator3-dev
    pkg_need openssl libssl-dev
    pkg_need libsoup-3.0 libsoup-3.0-dev
    pkg_need libxdo libxdo-dev
    # libgbm：缺了链接期报 `unable to find library -lgbm`
    pkg_need gbm libgbm-dev
    # libspa-sys（tauri-plugin-mcp → xcap → pipewire）：需要 pipewire 头文件
    # 以及 bindgen 用的 clang/libclang（缺 clang 时报 `stdbool.h file not found`）
    pkg_need libpipewire-0.3 libpipewire-0.3-dev
    command -v clang >/dev/null 2>&1 || need+=(clang)
    ls /usr/lib/llvm-*/lib/libclang.so* /usr/lib/x86_64-linux-gnu/libclang*.so* >/dev/null 2>&1 || need+=(libclang-dev)
    if [ "${#need[@]}" -gt 0 ]; then
      echo "==> sudo apt-get install -y ${need[*]} ..."
      sudo apt-get update -qq || true
      sudo apt-get install -y "${need[@]}" || echo "    !! apt 安装失败，请手动运行上面的命令"
    else
      echo "    运行时与构建依赖均已就绪（tesseract 未勾选或已安装）"
    fi
  else
    echo "==> 非 apt 发行版，请用系统包管理器安装："
    echo "    poppler-utils、ffmpeg${INCLUDE_TESSERACT:+、tesseract-ocr}"
    echo "    以及 Tauri 构建依赖：WebKitGTK 4.1 / GTK3 / librsvg / ayatana-appindicator3 /"
    echo "    libssl / libsoup3 / libxdo / libgbm / libpipewire-0.3 的开发包，外加 clang（bindgen）"
  fi
else
  echo "    !! 未知系统 $OS，跳过系统依赖安装"
fi

# 6. (可选) 模型发布资产：设置环境变量以指向 GitHub 模型仓库
echo ""
echo "==> 完成。首次 cargo build 会从 GitHub 拉 tauri-plugin-mcp 与 sherpa-onnx 预编译库，"
echo "    请确保 git 代理可用，或设置："
echo "    export HTTPS_PROXY=http://127.0.0.1:7890"
echo ""
echo "    git 依赖被重置时可改用镜像重写（cargo 已配 git-fetch-with-cli）："
echo "    git config --global url.\"https://ghfast.top/https://github.com/\".insteadOf \"https://github.com/\""
echo ""
echo "    sherpa-onnx 预编译库（Linux 为 sherpa-onnx-v<版本>-linux-x64-static-lib.tar.bz2）"
echo "    可先手动下载到本地目录，再用 SHERPA_ONNX_ARCHIVE_DIR 指向它，跳过构建期下载。"
echo ""
echo "    模型下载默认源：github.com/linksearcher/link-searcher-models（发布版首启自动镜像下载）"
echo "    如需覆盖：export LINK_SEARCHER_MODELS_GH=yourname/repo LINK_SEARCHER_MODELS_TAG=models-v1"
echo ""
echo "    虚拟机 / 无 GPU 环境（Linux）若窗口全白，启动时加环境变量："
echo "      WEBKIT_DISABLE_DMABUF_RENDERER=1（或 WEBKIT_DISABLE_COMPOSITING_MODE=1）"
