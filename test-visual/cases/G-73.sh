#!/bin/bash
# G-73: 点击扫描→进度条 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-73" "点击扫描→进度条"
goto "/index"
clicktext "开始扫描"
sleep 2
expect_count "document.body.innerText.includes('扫描中')||document.body.innerText.includes('索引中')||document.body.innerText.includes('进度') ? 1 : 0" "扫描进度出现"
case_end
