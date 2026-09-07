#!/bin/bash
# G-92: 主题切换→跟随系统 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-92" "主题跟随系统"
goto "/"; clicktext "跟随系统"; sleep 1
expect_count "document.documentElement.classList.length" "跟随系统生效"
case_end
