#!/bin/bash
# G-133: 追问换范围 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-133" "追问换范围"
goto "/chat"
expect_count "document.body.innerText.length" "AI聊天页"
case_end
