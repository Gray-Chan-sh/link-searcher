#!/bin/bash
# G-11: 关键词搜索→结果出现 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-11" "关键词搜索→结果出现"
goto "/"
type "合同" "input[data-search-input=true]"
clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果加载"
expect_count "document.body.innerText.match(/\\d+\\s*条结果/) ? 1 : 0" "搜索结果出现"
case_end
