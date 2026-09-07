#!/bin/bash
# G-99: OCR语言切换 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-99" "OCR语言切换"
goto "/settings"
expect_count "document.body.innerText.length" "OCR语言设置"
case_end
