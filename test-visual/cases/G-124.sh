#!/bin/bash
# G-124: 回答含引用 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-124" "回答含引用"
goto "/chat"
expect_count "document.body.innerText.length" "AI聊天页"
case_end
