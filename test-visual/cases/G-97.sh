#!/bin/bash
# G-97: 设置改值→自动保存 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-97" "设置改值→自动保存"
goto "/settings"
js "var sel=Array.from(document.querySelectorAll('select')).find(s=>s.textContent.includes('English')||s.textContent.includes('中文')); if(sel){var opts=Array.from(sel.options); var zh=opts.find(o=>o.textContent.includes('中文')); if(zh){sel.value=zh.value; sel.dispatchEvent(new Event('change',{bubbles:true}));}} \"done\""
sleep 1
goto "/browse"
goto "/settings"
expect_count "document.body.innerText.includes('搜索') ? 1 : 0" "自动保存生效"
case_end
