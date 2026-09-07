#!/bin/bash
# G-30: 清空筛选→恢复全量 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-30" "清空筛选"
goto "/"; type "法院" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果"
clicktext "清空筛选"
sleep 1
expect_count "document.body.innerText.length" "清空筛选执行"
case_end
