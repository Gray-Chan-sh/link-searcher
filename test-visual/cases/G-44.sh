#!/bin/bash
# G-44: 类型筛选→选PDF [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-44" "浏览类型筛选"
goto "/browse"
expect_count "document.body.innerText.length" "浏览页可用"
case_end
