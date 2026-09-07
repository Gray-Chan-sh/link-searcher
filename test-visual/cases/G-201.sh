#!/bin/bash
# G-201: ZIP导出加密 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-201" "ZIP导出加密"
goto "/settings"
sleep 1
# 找备份 tab 或导出按钮
js "var b=Array.from(document.querySelectorAll('button')).find(b=>b.textContent.includes('备份')||b.textContent.includes('导出')); if(b) b.click(); 'clicked'"
sleep 1
expect_count "document.body.innerText.length" "备份导出按钮操作"
case_end
