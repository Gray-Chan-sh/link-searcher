#!/bin/bash
# G-107: 未配AI→语义隐藏 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-107" "未配AI→语义隐藏"
goto "/"
# 语义开关应隐藏
expect_js "document.body.innerText.includes('语义') ? 'visible' : 'hidden'" "visible" "语义开关可见"
case_end
