#!/bin/bash
# G-20: 短语搜索 "dog cat" [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-20" "短语搜索"
goto "/"
type "法院 传票" "input[data-search-input=true]"
clickcss "button[aria-label=搜索]"
sleep 2
expect_count "document.body.innerText.length" "短语搜索执行"
case_end
