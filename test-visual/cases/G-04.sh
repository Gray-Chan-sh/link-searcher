#!/bin/bash
# G-04: 状态栏显示索引计数 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-04" "状态栏显示索引计数"
goto "/"
expect_count "document.body.innerText.match(/已索引\\s*\\d+/) ? 1 : 0" "状态栏有已索引计数"
case_end
