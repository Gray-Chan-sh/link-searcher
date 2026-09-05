#!/usr/bin/env bash
# Link-Searcher 开发环境一键清理（macOS / Linux）
#
# 清理 setup-dev.sh 产生的构建产物，让源码回到初始状态（保留系统级配置
# 如 cargo/npm 镜像、lld-link、brew/apt 装的 bin 工具，它们可能被其它
# 项目共用，不在此清理范围）。
#
# 清理内容：
#   - src-tauri/target/           Rust 编译产物（含解压的 sherpa-onnx）
#   - node_modules/               npm ci 产物
#   - dist/ dist-ssr/             vite 构建产物
#   - third_party/sherpa-onnx/    Windows 专用 sherpa 下载缓存（macOS 一般无）
#
# 保留：~/.cargo/config.toml、项目 .npmrc、src-tauri/.cargo/config.toml、
#       数据目录模型（应用运行时数据，非构建产物）。
#
# 参数：
#   -y  跳过确认直接删
#
# 幂等：可重复执行。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSUME_YES=0
[ "${1:-}" = "-y" ] && ASSUME_YES=1

TARGETS=(
  "$ROOT/src-tauri/target"
  "$ROOT/node_modules"
  "$ROOT/dist"
  "$ROOT/dist-ssr"
  "$ROOT/third_party/sherpa-onnx"
)

echo "==> Link-Searcher dev clean (root: $ROOT)"
echo "    将删除以下构建产物（系统级配置与数据目录模型保留）："
for t in "${TARGETS[@]}"; do
  [ -e "$t" ] && echo "    - $t"
done
echo "    清空数据目录下的模型（PaddleOCR/BGE/FunASR ~965MB）请手动删："
echo "      rm -rf \"\$HOME/Library/Application Support/link-searcher/models\""

if [ "$ASSUME_YES" != "1" ]; then
  read -r -p "确认删除？[y/N] " ans
  [ "$ans" = "y" ] || [ "$ans" = "Y" ] || { echo "已取消"; exit 0; }
fi

for t in "${TARGETS[@]}"; do
  if [ -e "$t" ]; then
    rm -rf "$t"
    echo "    已删除: $t"
  fi
done

echo ""
echo "==> 完成。之后重新就绪："
echo "    ./scripts/setup-dev.sh"
echo "    npm ci"
echo "    cd src-tauri && cargo build"