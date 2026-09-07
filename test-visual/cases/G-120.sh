#!/bin/bash
# G-120: 聊天页加载→输入框可见 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-120" "聊天页加载输入框"
goto "/chat"
expect_count "document.querySelectorAll('textarea, input[type=text]').length" "聊天输入框存在"
case_end
