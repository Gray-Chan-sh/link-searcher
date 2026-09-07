#!/bin/bash
# G-13: 结果显示文件名/路径/类型 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-13" "结果显示文件信息"
goto "/"; type "法院" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果"
expect_count "document.body.innerText.includes('.md')||document.body.innerText.includes('.pdf')||document.body.innerText.includes('.docx')||document.body.innerText.includes('.jpg') ? 1 : 0" "结果显示文件类型"
expect_count "document.body.innerText.includes('得分') ? 1 : 0" "结果显示得分"
case_end
