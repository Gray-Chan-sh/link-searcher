#!/bin/bash
# G-60: 点击结果→预览出现 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-60" "点击结果→预览出现"
goto "/"; type "法院" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果"
clicktext "得分"
sleep 1
expect_count "
(function(){var all=Array.from(document.querySelectorAll('div')); var pv=all.filter(d=>{var r=d.getBoundingClientRect(); return r.left>800&&r.width>50&&d.textContent.length>30}); return pv.length;})()
" "预览面板出现"
case_end
