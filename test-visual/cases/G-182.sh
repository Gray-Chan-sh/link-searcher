#!/bin/bash
# G-182: 浏览多选→AI问答 [P1]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-182" "浏览多选AI问答"
goto "/browse"
expect_count "document.body.innerText.length" "浏览页渲染"
case_end
