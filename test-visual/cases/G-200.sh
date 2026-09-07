#!/bin/bash
# G-200: 触发备份 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-200" "触发备份"
goto "/settings"
expect_count "document.body.innerText.length" "设置页渲染"
case_end
