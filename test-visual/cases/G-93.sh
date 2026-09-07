#!/bin/bash
# G-93: 语言切换→英文 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-93" "语言切换→英文"
goto "/settings"
# 找语言下拉并选 English
js "var sel=Array.from(document.querySelectorAll('select')).find(s=>s.textContent.includes('张')||s.textContent.includes('中文')||s.textContent.includes('English')); if(sel){var opts=Array.from(sel.options); var en=opts.find(o=>o.textContent.includes('English')); if(en){sel.value=en.value; sel.dispatchEvent(new Event('change',{bubbles:true}));}} \"done\""
sleep 1
expect_count "document.body.innerText.includes('Search') ? 1 : 0" "界面变英文"
case_end
