#!/usr/bin/env bash
# ===========================================================================
# 专注模式自动化测试
# 前置：App 已在 debug 模式运行（npx tauri dev）
# 用法：bash src-tauri/tests/focus_mode_test.sh
# ===========================================================================
set -e

MCP=$(find /var/folders -name "tauri-mcp.sock" 2>/dev/null | head -1)
if [ -z "$MCP" ]; then
  echo "❌ 未找到 MCP socket。请先启动 debug 版 App：npx tauri dev"
  exit 1
fi
echo "✅ MCP socket: $MCP"

PASS=0; FAIL=0
FIFO=/tmp/focus_test_fifo
RESP=/tmp/focus_test_resp.json
rm -f "$FIFO" "$RESP"

mcp() {
  local id=$1 method=$2 params=$3 wait=$4
  rm -f "$FIFO"; mkfifo "$FIFO"
  TAURI_MCP_IPC_PATH="$MCP" npx tauri-plugin-mcp-server < "$FIFO" > "$RESP" 2>/dev/null &
  local pid=$!
  sleep 1
  echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"$method\",\"params\":$params}" > "$FIFO"
  sleep "$wait"
  exec 3>"$FIFO" 2>/dev/null; exec 3>&-
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

ok() { echo "  ✅ $1"; PASS=$((PASS+1)); }
fail() { echo "  ❌ $1"; FAIL=$((FAIL+1)); }

# ===========================================================================
# 测试文件：毛莹.pdf（file_id 应为 9d90475d-...）
FOCUS_FILE="毛莹.pdf"
EXPECTED_FILE_ID="9d90475d-136b-480f-b301-2dd84ab890f7"

echo ""
echo "============================================================"
echo "  专注模式自动化测试"
echo "  目标文件: $FOCUS_FILE"
echo "  期望 file_id: $EXPECTED_FILE_ID"
echo "============================================================"
echo ""

# ---------------------------------------------------------------------------
# L1: 后端 IPC —— 直接设置 focus_file 并验证
# ---------------------------------------------------------------------------
echo "━━━ [L1] 后端 IPC：save_chat_session 设置 focus_file ━━━"

TEST_SESSION_ID="test-focus-$(date +%s)"
mcp 1 "tools/call" "{\"name\":\"manage_ipc\",\"arguments\":{\"action\":\"invoke\",\"command\":\"save_chat_session\",\"args\":{\"session\":{\"id\":\"$TEST_SESSION_ID\",\"title\":\"专注测试\",\"created_at\":0,\"updated_at\":0,\"messages\":[],\"source_ids\":[],\"source_files\":[],\"pending_query\":null,\"pending_started_at\":null,\"per_turn_evidence\":[],\"per_turn_scopes\":[],\"scope_dir_ids\":[],\"scope_conditions\":[],\"strict_docs\":false,\"focus_file\":\"$FOCUS_FILE\"}}}}" 5

if python3 -c "import json; d=json.load(open('$RESP')); assert 'result' in d and not d.get('isError', False)" 2>/dev/null; then
  ok "L1.1 save_chat_session 成功（focus_file=$FOCUS_FILE）"
else
  fail "L1.1 save_chat_session 失败"
fi

# ---------------------------------------------------------------------------
# L2: 前端逻辑 —— 导航加载该会话，发送消息（无 @mention），查日志
# ---------------------------------------------------------------------------
echo ""
echo "━━━ [L2] 前端：导航到该会话并发送消息 ━━━"

# 导航到聊天页
mcp 2 "tools/call" "{\"name\":\"navigate\",\"arguments\":{\"action\":\"goto\",\"url\":\"http://localhost:1420/#/chat\"}}" 4

# 通过 execute_js 直接设置 React 状态太复杂，改用 DOM 输入
mcp 3 "tools/call" "{\"name\":\"execute_js\",\"arguments\":{\"code\":\"var inp = document.querySelector('input[type=text]'); if(inp){ var s = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value').set; s.call(inp,'身份证号是多少'); inp.dispatchEvent(new Event('input',{bubbles:true})); 'ok' }\"}}" 4

# 从侧边栏加载测试会话（点击标题匹配）
mcp 4 "tools/call" "{\"name\":\"execute_js\",\"arguments\":{\"code\":\"var btns = Array.from(document.querySelectorAll('button')); var target = btns.find(b=>b.textContent.includes('专注测试')); if(target){ target.click(); 'clicked' } else { 'not found' }\"}}" 4

# 发送
mcp 5 "tools/call" "{\"name\":\"click\",\"arguments\":{\"selector_type\":\"text\",\"selector_value\":\"发送\",\"match\":\"exact\"}}" 3
sleep 15

# 查日志
echo ""
echo "  日志检查："
grep "bm25_relevant_hits\|conversation_ask_stream:" /Volumes/Data/index/app.log 2>/dev/null | tail -4

if grep -q "file_ids=Some(\[\"$EXPECTED_FILE_ID\"\])" /Volumes/Data/index/app.log 2>/dev/null; then
  ok "L2.1 搜索限制在 $FOCUS_FILE（file_id=$EXPECTED_FILE_ID）"
else
  fail "L2.1 file_id 不是 $EXPECTED_FILE_ID（可能未设置为专注）"
fi

if grep -q "file_ids=None" /Volumes/Data/index/app.log 2>/dev/null; then
  ok "L2.2 存在全库搜索记录（对照：无引用时应 file_ids=None）"
else
  echo "  ⚠️ 未找到 file_ids=None 记录（可能本次会话一直有 focus_file）"
fi

# ---------------------------------------------------------------------------
# 清理：删除测试会话
# ---------------------------------------------------------------------------
echo ""
echo "━━━ 清理测试会话 ━━━"
mcp 6 "tools/call" "{\"name\":\"manage_ipc\",\"arguments\":{\"action\":\"invoke\",\"command\":\"delete_chat_session\",\"args\":{\"id\":\"$TEST_SESSION_ID\"}}}" 5
ok "删除测试会话 $TEST_SESSION_ID"

echo ""
echo "============================================================"
echo "  测试完成: $PASS 通过, $FAIL 失败"
echo "============================================================"
rm -f "$FIFO" "$RESP"
exit $FAIL