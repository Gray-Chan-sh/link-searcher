#!/bin/bash
# G-128: @引用文件 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-128" "@引用文件"
goto "/chat"
expect_count "document.querySelectorAll('textarea, input[type=text]').length" "聊天页可用"
case_end
