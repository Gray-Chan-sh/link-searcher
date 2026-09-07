#!/bin/bash
# G-160: 类型列表加载 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-160" "类型列表加载"
goto "/file-types"
expect_count "document.body.innerText.includes('文件类型') ? 1 : 0" "文件类型页渲染"
case_end
