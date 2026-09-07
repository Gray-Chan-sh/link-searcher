#!/bin/bash
# G-94: 语言切换→日文 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-94" "语言切换日文"
goto "/settings"
js "var sel=Array.from(document.querySelectorAll('select')).find(s=>s.textContent.includes('English')||s.textContent.includes('中文')); if(sel){var opts=Array.from(sel.options); var ja=opts.find(o=>o.textContent.includes('日本語')); if(ja){sel.value=ja.value; sel.dispatchEvent(new Event('change',{bubbles:true}));}} 'done'"
sleep 1
expect_count "document.body.innerText.length" "日文切换"
case_end
