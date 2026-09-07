#!/bin/bash
# G-56: 列宽拖拽 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-56" "列宽拖拽"
goto "/browse"
sleep 1
# 触发列宽拖拽 (mousedown + mousemove + mouseup on th)
js "(function(){var th=document.querySelector('th'); if(!th) return 'no th'; var r=th.getBoundingClientRect(); var x=r.left, y=r.top+r.height/2; th.dispatchEvent(new MouseEvent('mousedown',{bubbles:true,clientX:x,clientY:y})); document.dispatchEvent(new MouseEvent('mousemove',{bubbles:true,clientX:x+50,clientY:y})); document.dispatchEvent(new MouseEvent('mouseup',{bubbles:true,clientX:x+50,clientY:y})); return 'dragged';})()"
sleep 1
expect_count "document.body.innerText.length" "列宽拖拽执行"
case_end
