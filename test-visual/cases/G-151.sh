#!/bin/bash
# G-151: 类型筛选→OCR [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-151" "日志类型筛选OCR"
goto "/logs"
expect_count "document.body.innerText.length" "日志页渲染"
case_end
