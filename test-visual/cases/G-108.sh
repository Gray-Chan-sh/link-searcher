#!/bin/bash
# G-108: 排除规则编辑 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-108" "排除规则编辑"
goto "/settings"
sleep 1
# 在排除规则 textarea 输入 glob
js "var t=document.querySelector('textarea'); if(t){var s=Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set; s.call(t,'*.bak\n#temp'); t.dispatchEvent(new Event('input',{bubbles:true}));} 'typed'"
sleep 1
expect_count "document.body.innerText.length" "排除规则编辑"
case_end
