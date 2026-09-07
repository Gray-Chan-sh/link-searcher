#!/bin/bash
# G-16: 中英混合搜索 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-16" "中英混合搜索"
goto "/"; type "合同 contract" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
sleep 2
expect_count "document.body.innerText.length" "中英混合搜索执行"
case_end
