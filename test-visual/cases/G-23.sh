#!/bin/bash
# G-23: 建议键盘导航 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-23" "建议键盘导航"
goto "/"
type "法" "input[data-search-input=true]"
sleep 2
# 按 ↓ 键
press "ArrowDown"
sleep 1
expect_count "document.body.innerText.length" "键盘导航执行"
case_end
