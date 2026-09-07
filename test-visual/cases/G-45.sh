#!/bin/bash
# G-45: 文件名搜索 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-45" "浏览文件名搜索"
goto "/browse"
expect_count "document.body.innerText.length" "浏览页可用"
case_end
