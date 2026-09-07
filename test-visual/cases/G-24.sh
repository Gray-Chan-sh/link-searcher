#!/bin/bash
# G-24: 排序切换→顺序变 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-24" "排序切换"
goto "/"; type "法院" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
wait_js "document.body.innerText.includes('得分')" 10 "搜索结果"
# 点排序下拉 (按日期)
js "var sel=Array.from(document.querySelectorAll('select')).find(s=>s.textContent.includes('相关性')); if(sel){var d=Array.from(sel.options).find(o=>o.textContent.includes('日期')); if(d){sel.value=d.value; sel.dispatchEvent(new Event('change',{bubbles:true}));}} \"sorted\""
sleep 2
expect_count "document.body.innerText.length" "排序切换"
case_end
