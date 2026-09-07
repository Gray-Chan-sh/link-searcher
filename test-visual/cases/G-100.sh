#!/bin/bash
# G-100: OCR引擎测试 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-100" "OCR引擎测试"
goto "/settings"
clicktext "测试 OCR"
sleep 2
expect_count "document.body.innerText.length" "OCR测试"
case_end
