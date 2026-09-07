#!/bin/bash
# G-32: CSV导出按钮存在 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-32" "CSV导出按钮"
goto "/"; type "法院" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果"
expect_count "Array.from(document.querySelectorAll('button')).some(b=>b.textContent.includes('导出')) ? 1 : 0" "导出CSV按钮存在"
case_end
