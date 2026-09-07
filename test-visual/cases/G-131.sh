#!/bin/bash
# G-131: 会话导出 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-131" "会话导出"
goto "/chat"
expect_count "document.querySelectorAll('textarea, input[type=text]').length" "聊天页渲染"
case_end
