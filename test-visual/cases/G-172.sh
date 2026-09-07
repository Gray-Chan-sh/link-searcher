#!/bin/bash
# G-172: 任务简报→跳转 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-172" "任务简报"
goto "/"
expect_count "document.body.innerText.length" "状态栏可见"
case_end
