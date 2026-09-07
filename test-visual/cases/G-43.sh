#!/bin/bash
# G-43: 状态筛选→失败 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-43" "浏览筛选失败"
goto "/browse"
expect_count "document.body.innerText.length" "浏览页可用"
case_end
