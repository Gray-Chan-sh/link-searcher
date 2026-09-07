#!/bin/bash
# G-154: 会话日志切换 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-154" "会话日志切换"
goto "/logs"
expect_count "document.body.innerText.length" "日志页渲染"
case_end
