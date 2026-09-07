#!/bin/bash
# G-02: 侧边栏8个导航项 [P0]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-02" "侧边栏8个导航项"
for hash in "/" "/browse" "/directories" "/index" "/file-types" "/settings" "/logs" "/chat"; do
  goto "$hash"
  expect_count "document.body.innerText.length" "导航到 $hash"
done
case_end
