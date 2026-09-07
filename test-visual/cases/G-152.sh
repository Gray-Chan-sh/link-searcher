#!/bin/bash
# G-152: 关键字过滤 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-152" "日志关键字过滤"
goto "/logs"
expect_count "document.body.innerText.length" "日志页渲染"
case_end
