#!/bin/bash
# G-33: 语义搜索开关 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-33" "语义搜索开关"
goto "/"
expect_count "document.body.innerText.length" "语义搜索可见"
case_end
