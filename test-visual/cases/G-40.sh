#!/bin/bash
# G-40: 表格加载→显示文件列表 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-40" "浏览页文件列表"
goto "/browse"
sleep 2
expect_count "document.body.innerText.length" "浏览页渲染"
case_end
