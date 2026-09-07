#!/bin/bash
# G-140: 目录列表加载 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-140" "目录列表加载"
goto "/directories"
expect_count "document.body.innerText.length" "目录页渲染"
case_end
