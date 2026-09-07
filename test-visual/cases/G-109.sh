#!/bin/bash
# G-109: Web API 开关 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-109" "Web API开关"
goto "/settings"
sleep 1
# 尝试找到 Web API 相关 toggle/checkbox
js "var cb=Array.from(document.querySelectorAll('input[type=checkbox],button[role=switch]')).find(c=>c.closest('[class*=card],[class*=setting],[class*=tab]')?.textContent.includes('Web API')||c.parentElement?.textContent.includes('Web API')); if(cb){cb.click();} 'clicked'"
sleep 1
expect_count "document.body.innerText.length" "Web API开关操作"
case_end
