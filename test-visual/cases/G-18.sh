#!/bin/bash
# G-18: 模糊搜索 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-18" "模糊搜索"
goto "/"; type "法徃" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
sleep 2
expect_count "document.body.innerText.length" "模糊搜索执行"
case_end
