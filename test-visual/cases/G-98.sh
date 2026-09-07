#!/bin/bash
# G-98: OCR引擎切换 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-98" "OCR引擎切换"
goto "/settings"
expect_count "document.body.innerText.length" "设置页OCR区域"
case_end
