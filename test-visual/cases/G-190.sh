#!/bin/bash
# G-190: CmdK聚焦搜索框 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-190" "CmdK聚焦搜索框"
goto "/"
expect_count "document.body.innerText.length" "页面可用"
case_end
