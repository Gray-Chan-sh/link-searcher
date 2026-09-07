#!/bin/bash
# G-202: ZIP恢复 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-202" "ZIP恢复"
goto "/settings"
expect_count "document.body.innerText.length" "设置页渲染（恢复走原生对话框）"
case_end
