#!/bin/bash
# G-22: 搜索建议 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-22" "搜索建议"
goto "/"
type "法" "input[data-search-input=true]"
sleep 2
expect_count "document.querySelectorAll('li, [role=option]').length" "建议下拉出现"
case_end
