#!/bin/bash
# G-181: 索引统计→跳转浏览 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-181" "索引统计跳转浏览"
goto "/index"
expect_count "document.body.innerText.length" "索引页渲染"
case_end
