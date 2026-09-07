#!/bin/bash
# G-32: CSV导出——验证搜索结果页可见 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-32" "搜索结果显示"
goto "/"; type "法院" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果"
expect_count "document.body.innerText.length" "搜索结果页渲染"
case_end
