#!/bin/bash
# G-191: ↑↓结果选择 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-191" "上下结果选择"
goto "/"; type "法院" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果"
press "ArrowDown"
sleep 1
press "ArrowUp"
sleep 1
expect_count "document.body.innerText.length" "上下选择"
case_end
