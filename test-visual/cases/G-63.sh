#!/bin/bash
# G-63: PDF预览 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-63" "PDF预览"
goto "/"; type "pdf" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
sleep 2
expect_count "document.body.innerText.length" "PDF搜索"
case_end
