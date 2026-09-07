#!/bin/bash
# G-101: 语义权重滑杆 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-101" "语义权重滑杆"
goto "/settings"
expect_count "document.body.innerText.length" "语义权重"
case_end
