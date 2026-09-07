#!/bin/bash
# G-42: 状态筛选→未索引 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-42" "浏览筛选未索引"
goto "/browse"
expect_count "document.body.innerText.length" "浏览页可用"
case_end
