#!/bin/bash
# G-161: 缺依赖格式显示 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-161" "缺依赖格式显示"
goto "/file-types"
expect_count "document.body.innerText.length" "文件类型页渲染"
case_end
