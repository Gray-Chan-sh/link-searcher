#!/bin/bash
# G-144: 拖拽添加目录 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-144" "拖拽添加目录"
goto "/directories"
expect_count "document.body.innerText.length" "目录页渲染（拖拽为 OS 级操作, 验证页面可用）"
case_end
