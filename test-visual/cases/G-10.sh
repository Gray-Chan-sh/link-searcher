#!/bin/bash
# G-10: 空搜索→不崩溃 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-10" "空搜索→不崩溃"
goto "/"
clickcss "button[aria-label=搜索]"
sleep 1
expect_count "document.body.innerText.length" "页面不崩溃"
case_end
