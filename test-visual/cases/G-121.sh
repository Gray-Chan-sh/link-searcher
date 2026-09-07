#!/bin/bash
# G-121: 新建会话 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-121" "新建会话"
goto "/chat"
expect_count "document.body.innerText.length" "聊天页加载"
case_end
