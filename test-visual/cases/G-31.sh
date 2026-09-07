#!/bin/bash
# G-31: 筛选刷新后保持 [P2]
set -u; TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"; source "$TEST_VISUAL_DIR/lib.sh"
case_begin "G-31" "筛选刷新后保持"
goto "/"
js "var cb=document.querySelector('input[type=checkbox]'); if(cb) cb.click(); 'clicked'" >/dev/null
sleep 1
before="$(js "localStorage.getItem('ls_filter_dirs')||'none'")"
goto "/browse" >/dev/null
goto "/" >/dev/null
sleep 1
after="$(js "localStorage.getItem('ls_filter_dirs')||'none'")"
echo "before='$before' after='$after'"
if [ "$before" = "$after" ]; then
  echo "  ✅ 筛选状态刷新后保持"
else
  echo "  ❌ 筛选状态变化 (before=$before after=$after)"
  ASSERT_FAILS_CUR=1
fi
case_end
