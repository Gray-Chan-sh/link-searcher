#!/bin/bash
# G-180: 搜索→缩小范围 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-180" "搜索缩小范围"
goto "/"
type "法院" "input[data-search-input=true]"
clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果"
expect_count "document.body.innerText.length" "搜索结果页"
case_end
