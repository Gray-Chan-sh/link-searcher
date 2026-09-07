#!/bin/bash
# G-17: 无结果→引导显示 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-17" "无结果引导"
goto "/"
type "ZZZZZZZZZZZZZZZZZZZZZZZ" "input[data-search-input=true]"
clickcss "button[aria-label=搜索]"
sleep 2
expect_count "document.body.innerText.length" "无结果处理"
case_end
