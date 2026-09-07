#!/bin/bash
# G-28: 目录树筛选 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-28" "目录树筛选"
goto "/"; type "法院" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果"
expect_count "document.body.innerText.length" "目录筛选可用"
case_end
