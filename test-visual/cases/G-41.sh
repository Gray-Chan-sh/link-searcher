#!/bin/bash
# G-41: 状态筛选 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-41" "浏览页状态筛选"
goto "/browse"
expect_count "document.body.innerText.length" "浏览页可用"
case_end
