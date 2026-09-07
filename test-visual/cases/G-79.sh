#!/bin/bash
# G-79: 重建索引→确认框 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-79" "重建索引确认"
goto "/index"
expect_count "document.body.innerText.length" "索引页渲染"
case_end
