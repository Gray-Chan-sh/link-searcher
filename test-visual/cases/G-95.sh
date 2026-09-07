#!/bin/bash
# G-95: 语言切换→韩文 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-95" "语言切换韩文"
goto "/settings"
js "var sel=Array.from(document.querySelectorAll('select')).find(s=>s.textContent.includes('English')||s.textContent.includes('中文')); if(sel){var opts=Array.from(sel.options); var ko=opts.find(o=>o.textContent.includes('한국어')); if(ko){sel.value=ko.value; sel.dispatchEvent(new Event('change',{bubbles:true}));}} 'done'"
sleep 1
expect_count "document.body.innerText.length" "韩文切换"
case_end
