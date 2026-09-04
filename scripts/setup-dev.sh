#!/usr/bin/env bash
# Link-Searcher 开发环境一键配置（macOS / Linux）
#
# 用途：clone 源码后在开发机快速就绪，无需手工配置镜像。
#   - 配置 cargo 国内镜像（rsproxy.cn，可改环境变量 CARGO_REGISTRY_MIRROR）
#   - 配置 npm 镜像（npmmirror.com，可改 NPM_REGISTRY_MIRROR）
#   - 让 git 依赖（tauri-plugin-mcp 等）复用系统 git 的代理/凭证
#   - 提示安装平台系统依赖（Homebrew / apt）
#   - 提示把模型发布资产下载到 dev 目录（可选）
#
# 幂等：可重复执行。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_REGISTRY_MIRROR="${CARGO_REGISTRY_MIRROR:-rsproxy-sparse}"
NPM_REGISTRY_MIRROR="${NPM_REGISTRY_MIRROR:-https://registry.npmmirror.com}"

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

# 5. 平台系统依赖
case "$(uname -s)" in
  Darwin)
    if command -v brew >/dev/null; then
      echo "==> 检查 Homebrew 依赖..."
      brew list --formula 2>/dev/null | grep -qx "poppler" || echo "    macOS 建议安装: brew install poppler"
      brew list --formula 2>/dev/null | grep -qx "ffmpeg" || echo "    macOS 建议安装: brew install ffmpeg"
      brew list --formula 2>/dev/null | grep -qx "tesseract" || echo "    macOS 建议安装: brew install tesseract tesseract-lang"
    else
      echo "    !! 未检测到 Homebrew。安装 poppler/ffmpeg/tesseract 需要它。"
    fi
    ;;
  Linux)
    echo "==> Linux 建议安装（Debian/Ubuntu）:"
    echo "    sudo apt install -y libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev \\"
    echo "      libxdo-dev libappindicator3-dev librsvg2-dev libpipewire-0.3-dev \\"
    echo "      libspa-0.2-dev libgbm-dev patchelf poppler-utils ffmpeg tesseract-ocr"
    ;;
esac

# 6. (可选) 模型发布资产：设置环境变量以指向 GitHub 模型仓库
echo ""
echo "==> 完成。首次 cargo build 会从 GitHub 拉 tauri-plugin-mcp 与 sherpa-onnx 预编译库，"
echo "    请确保 git 代理可用，或设置："
echo "    export HTTPS_PROXY=http://127.0.0.1:7890"
echo ""
echo "    模型下载默认源：github.com/linksearcher/link-searcher-models（发布版首启自动镜像下载）"
echo "    如需覆盖：export LINK_SEARCHER_MODELS_GH=yourname/repo LINK_SEARCHER_MODELS_TAG=models-v1"
