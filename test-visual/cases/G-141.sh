#!/bin/bash
# G-141: 添加目录 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-141" "添加目录"
goto "/directories"
expect_count "document.body.innerText.length" "目录页可用"
case_end
