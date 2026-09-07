#!/bin/bash
# G-184: 任务简报→跳转日志 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-184" "任务简报跳转日志"
goto "/"
expect_count "document.body.innerText.length" "状态栏可见"
case_end
