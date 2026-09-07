#!/bin/bash
# G-27: 新查询→回第1页 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-27" "新查询回第1页"
goto "/"; type "合同" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
sleep 2
type "法院" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
sleep 2
expect_count "document.body.innerText.length" "新查询执行"
case_end
