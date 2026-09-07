#!/bin/bash
# G-96: 语言切换→中文 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-96" "语言切换→中文"
goto "/settings"
js "var sel=Array.from(document.querySelectorAll('select')).find(s=>s.textContent.includes('English')||s.textContent.includes('中文')); if(sel){var opts=Array.from(sel.options); var zh=opts.find(o=>o.textContent.includes('中文')); if(zh){sel.value=zh.value; sel.dispatchEvent(new Event('change',{bubbles:true}));}} \"done\""
sleep 1
expect_count "document.body.innerText.includes('搜索') ? 1 : 0" "界面变中文"
case_end
