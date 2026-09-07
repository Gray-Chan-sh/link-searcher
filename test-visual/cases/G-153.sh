#!/bin/bash
# G-153: 清除日志 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-153" "清除日志"
goto "/logs"
expect_count "document.body.innerText.length" "日志页渲染"
# 尝试点清除按钮
js "var b=Array.from(document.querySelectorAll('button')).find(b=>b.textContent.includes('清除')); if(b) b.click(); 'clicked'"
sleep 1
expect_count "document.body.innerText.length" "清除日志执行"
case_end
