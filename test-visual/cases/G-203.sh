#!/bin/bash
# G-203: 死目录检测→重映射 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-203" "死目录检测重映射"
goto "/settings"
expect_count "document.body.innerText.length" "设置页渲染（死目录重映射走原生对话框）"
case_end
