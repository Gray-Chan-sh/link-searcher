#!/bin/bash
# G-70: 统计卡片显示 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-70" "索引统计卡片"
goto "/index"
expect_count "document.body.innerText.length" "索引页渲染"
case_end
