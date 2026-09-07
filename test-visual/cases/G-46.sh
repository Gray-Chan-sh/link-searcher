#!/bin/bash
# G-46: 排序→按大小 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-46" "浏览排序按大小"
goto "/browse"
expect_count "document.body.innerText.length" "排序可用"
case_end
