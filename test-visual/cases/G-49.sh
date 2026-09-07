#!/bin/bash
# G-49: 分页→下一页 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-49" "分页→下一页"
goto "/browse"
sleep 1
js "var b=Array.from(document.querySelectorAll('button')).find(b=>b.textContent.includes('下一页')); if(b) b.click(); \"clicked\""
sleep 1
expect_count "document.body.innerText.length" "分页下一页"
case_end
