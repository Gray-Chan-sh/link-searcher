#!/usr/bin/env bash
# ===========================================================================
# AI 聊天流式对话 E2E 测试（MCP 方式）
# ===========================================================================
# 前提：App 已在 debug 模式运行（npx tauri dev），LLM 网关已配置
# 用法：bash src-tauri/tests/ai_chat_e2e_mcp.sh
# ===========================================================================
set -euo pipefail

MCP_SOCKET=$(find /var/folders -name "tauri-mcp.sock" 2>/dev/null | head -1)
if [ -z "$MCP_SOCKET" ]; then
  echo "❌ 未找到 MCP socket。请先启动 debug 版 App：npx tauri dev"
  exit 1
fi
echo "✅ MCP socket: $MCP_SOCKET"

PASS=0
FAIL=0
FIFO=$(mktemp -u /tmp/mcp_e2e_XXXX)
RESP=$(mktemp /tmp/mcp_resp_XXXX.json)

mcp_call() {
  local id=$1 method=$2 params=$3
  rm -f "$FIFO"
  mkfifo "$FIFO"
  TAURI_MCP_IPC_PATH="$MCP_SOCKET" npx tauri-plugin-mcp-server < "$FIFO" > "$RESP" 2>/dev/null &
  local pid=$!
  sleep 1
  echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"$method\",\"params\":$params}" > "$FIFO"
  sleep "$4"
  exec 3>"$FIFO"; exec 3>&-
  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
}

check_ok() {
  if python3 -c "import json; d=json.load(open('$RESP')); assert 'result' in d and not d.get('isError', False), str(d.get('error', d))" 2>/dev/null; then
    echo "  ✅ $1"
    ((PASS++))
  else
    echo "  ❌ $1"
    python3 -c "import json; d=json.load(open('$RESP')); print('    ' + json.dumps(d.get('error', d.get('result', {})), ensure_ascii=False)[:200])" 2>/dev/null
    ((FAIL++))
  fi
}

check_text_contains() {
  local label=$1 keyword=$2
  if python3 -c "
import json
d=json.load(open('$RESP'))
for item in d['result']['content']:
    if item.get('type') == 'text' and '$keyword' in item['text']:
        exit(0)
exit(1)
" 2>/dev/null; then
    echo "  ✅ $label"
    ((PASS++))
  else
    echo "  ❌ $label (未找到「$keyword」)"
    ((FAIL++))
  fi
}

echo ""
echo "============================================"
echo "  AI 聊天流式对话 E2E 测试"
echo "============================================"
echo ""

# ---- Test 1: 导航到 AI 聊天页 ----
echo "[Test 1] 导航到 AI 聊天页"
mcp_call 1 "tools/call" '{"name":"navigate","arguments":{"action":"goto","url":"http://localhost:1420/#/chat"}}' 3
check_ok "navigate 到 /#/chat"

# ---- Test 2: 确认页面状态 ----
echo "[Test 2] 确认页面状态"
mcp_call 2 "tools/call" '{"name":"query_page","arguments":{"mode":"state"}}' 3
check_text_contains "URL 包含 /#/chat" "/#/chat"

# ---- Test 3: 输入问题 ----
echo "[Test 3] 输入问题"
# 新会话输入框 placeholder 是"输入问题…"，追问是"继续追问…"，
# 用 [placeholder*="问题"] 或 [placeholder*="追问"] 均可命中。
mcp_call 3 "tools/call" '{"name":"type_text","arguments":{"text":"2025年营收增长了多少","selector_type":"css","selector_value":"input[placeholder*=\"问题\"], input[placeholder*=\"追问\"]"}}' 3
check_ok "输入框打字成功"

# ---- Test 4: 点击发送 ----
echo "[Test 4] 点击发送按钮"
mcp_call 4 "tools/call" '{"name":"click","arguments":{"selector_type":"text","selector_value":"发送","match":"exact"}}' 3
check_ok "发送按钮点击成功"

# ---- Test 5: 等待 AI 响应 ----
echo "[Test 5] 等待 AI 响应（最多 30 秒）"
FOUND=false
for i in $(seq 1 15); do
  sleep 2
  mcp_call 5 "tools/call" '{"name":"execute_js","arguments":{"code":"JSON.stringify({hasResponse: document.body.innerText.includes(\"2025年营收\"), hasThinking: document.body.innerText.includes(\"思考\") || document.body.innerText.includes(\"thinking\"), done: !document.body.innerText.includes(\"发送\") || !document.querySelector(\"button:last-child\")?.disabled})"}}' 3
  if python3 -c "
import json
d=json.load(open('$RESP'))
for item in d['result']['content']:
    if item.get('type') == 'text' and '\"done\":true' in item['text']:
        exit(0)
exit(1)
" 2>/dev/null; then
    FOUND=true
    echo "  ✅ AI 响应完成（约 $((i*2)) 秒）"
    ((PASS++))
    break
  fi
done
if [ "$FOUND" = false ]; then
  echo "  ❌ AI 响应超时（30 秒）"
  ((FAIL++))
fi

# ---- Test 6: 验证 AI 回答内容 ----
echo "[Test 6] 验证 AI 回答内容"
mcp_call 6 "tools/call" '{"name":"execute_js","arguments":{"code":"document.body.innerText.slice(-2000)"}}' 3
check_text_contains "AI 返回了回答" "营收"

# ---- Test 7: 验证响应耗时 ----
echo "[Test 7] 验证响应耗时标识"
mcp_call 7 "tools/call" '{"name":"execute_js","arguments":{"code":"document.body.innerText.includes(\"⏱\")"}}' 3
check_text_contains "显示 ⏱ 耗时" "⏱"

# ---- Test 8: 展开检索依据 ----
echo "[Test 8] 展开检索依据面板"
mcp_call 8 "tools/call" '{"name":"click","arguments":{"selector_type":"text","selector_value":"检索依据"}}' 3
check_ok "检索依据点击成功"

# ---- Test 9: 验证证据面板内容 ----
echo "[Test 9] 验证证据面板"
mcp_call 9 "tools/call" '{"name":"read_text","arguments":{"selector":"details","all":true,"max_chars":2000}}' 5
check_text_contains "证据面板包含来源数" "检索依据"

# ---- Test 10: 验证输入框已清空 ----
echo "[Test 10] 验证输入框已清空"
mcp_call 10 "tools/call" '{"name":"execute_js","arguments":{"code":"document.querySelector(\"input[type=text]\")?.value || \"\""}}' 3
if python3 -c "
import json
d=json.load(open('$RESP'))
for item in d['result']['content']:
    if item.get('type') == 'text' and item['text'].strip() in ['\"\"', '', '\"\"']:
        exit(0)
exit(1)
" 2>/dev/null; then
  echo "  ✅ 输入框已清空"
  ((PASS++))
else
  echo "  ❌ 输入框未清空"
  ((FAIL++))
fi

# ---- 结果汇总 ----
echo ""
echo "============================================"
echo "  结果: $PASS 通过, $FAIL 失败"
echo "============================================"
rm -f "$FIFO" "$RESP"
exit $FAIL