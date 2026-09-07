#!/bin/bash
# G-19: 通配符doc* [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-19" "通配符搜索"
goto "/"; type "法院*" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
sleep 2
expect_count "document.body.innerText.length" "通配符搜索执行"
case_end
