#!/bin/bash
# G-183: 聊天引用→跳转浏览 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-183" "聊天引用跳转"
goto "/chat"
expect_count "document.body.innerText.length" "聊天页渲染"
case_end
