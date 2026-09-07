#!/bin/bash
# G-170: 索引计数显示 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-170" "索引计数显示"
goto "/"
expect_count "document.body.innerText.match(/已索引\\s*\\d+/) ? 1 : 0" "底部状态栏索引计数"
case_end
