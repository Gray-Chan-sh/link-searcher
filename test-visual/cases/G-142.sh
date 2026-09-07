#!/bin/bash
# G-142: 编辑目录别名 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-142" "编辑目录别名"
goto "/directories"
expect_count "document.body.innerText.length" "目录页渲染"
case_end
