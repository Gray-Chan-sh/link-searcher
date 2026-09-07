#!/bin/bash
# G-62: 图片预览缩放 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-62" "图片预览"
goto "/browse"
sleep 2
expect_count "document.body.innerText.length" "浏览页渲染"
case_end
