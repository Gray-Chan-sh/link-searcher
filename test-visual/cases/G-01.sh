#!/bin/bash
# G-01: App启动→窗口出现 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-01" "App启动→窗口出现"
js "document.body.innerText.length > 0 ? 1 : 0" && expect_count "document.body.innerText.length" "页面有内容"
case_end
