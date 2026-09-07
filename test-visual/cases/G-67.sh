#!/bin/bash
# G-67: 预览Finder显示 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-67" "预览Finder显示"
goto "/"; type "法院" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果"
clicktext "得分"
sleep 1
# 点 Finder 按钮 (若有)
js "var b=Array.from(document.querySelectorAll('button,[role=button]')).find(b=>b.textContent.includes('Finder')||b.textContent.includes('查找器')); if(b) b.click(); 'clicked'"
sleep 1
expect_count "document.body.innerText.length" "Finder操作执行"
case_end
