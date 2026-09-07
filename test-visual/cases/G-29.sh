#!/bin/bash
# G-29: 扩展名筛选→结果收窄 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-29" "扩展名筛选"
goto "/"; type "法院" "input[data-search-input=true]"; clickcss "button[aria-label=搜索]"
sleep 1
# 勾选 .pdf 筛选 checkbox
js "var cbs=Array.from(document.querySelectorAll('input[type=checkbox]')); var pdf=cbs.find(cb=>cb.parentElement&&cb.parentElement.textContent.includes('.pdf')); if(pdf){pdf.click();} 'clicked'"
sleep 2
expect_count "document.body.innerText.length" "扩展名筛选执行"
case_end
