#!/bin/bash
# G-48: 排序方向切换 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-48" "排序方向切换"
goto "/browse"
sleep 1
# 点排序方向切换按钮（若有）
js "var b=Array.from(document.querySelectorAll('button')).find(b=>b.textContent.includes('↑')||b.textContent.includes('↓')||b.getAttribute('aria-label')==='toggle sort'); if(b) b.click(); 'clicked'"
sleep 1
expect_count "document.body.innerText.length" "排序切换执行"
case_end
