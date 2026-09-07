#!/bin/bash
# G-90: 主题切换 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-90" "主题切换"
goto "/"
clicktext "浅色"
sleep 1
expect_count "document.documentElement.classList.length" "主题切换生效"
case_end
