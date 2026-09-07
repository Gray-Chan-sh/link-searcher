#!/bin/bash
# G-14: 中文关键词搜索 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-14" "中文关键词搜索"
goto "/"
type "法院" "input[data-search-input=true]"
clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果加载"
expect_count "document.body.innerText.match(/\\d+\\s*条结果/) ? 1 : 0" "中文搜索有结果"
case_end
