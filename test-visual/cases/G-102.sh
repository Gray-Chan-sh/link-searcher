#!/bin/bash
# G-102: 添加AI Provider [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-102" "添加AI Provider"
goto "/settings"
expect_count "document.body.innerText.length" "AI设置"
case_end
