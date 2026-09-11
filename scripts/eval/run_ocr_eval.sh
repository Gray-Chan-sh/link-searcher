#!/usr/bin/env bash
# ===========================================================================
# OCR 提取质量评测（CER — Character Error Rate）
# 用法: bash scripts/eval/run_ocr_eval.sh
#
# 原理：
#   读取 ocr_fixtures/synthetic/manifest.jsonl，为每个条目生成合成测试图片
#   （若不存在），通过可用 OCR 引擎识别，与标注文本比对计算 CER。
#   单条 CER 超过 max_cer 即判定 FAIL；最终输出平均 CER。
#
# 注意：无 OCR 引擎可用时（模型缺失 / 无 tesseract），测试会优雅跳过而非失败。
# ===========================================================================
set -euo pipefail

echo "============================================"
echo "  OCR 提取质量评测（CER）"
echo "============================================"
echo ""

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "${SCRIPT_DIR}/../../src-tauri"

cargo test --test ocr_eval -- --nocapture

echo ""
echo "✅ OCR CER 评测完成"
