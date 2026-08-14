#!/usr/bin/env bash
# ===========================================================================
# Link-Searcher 全量 E2E 测试（MCP 方式）
# ===========================================================================
# 前提：App 已在 debug 模式运行（npx tauri dev），LLM 网关已配置
# 用法：bash src-tauri/tests/e2e_mcp.sh
# ===========================================================================
set -e
MCP=$(find /var/folders -name "tauri-mcp.sock" 2>/dev/null | head -1)
if [ -z "$MCP" ]; then echo "❌ 未找到 MCP socket。请先启动 debug 版 App：npx tauri dev"; exit 1; fi
echo "✅ MCP socket: $MCP"

PASS=0; FAIL=0

mcp() { rm -f /tmp/mcp_fifo; mkfifo /tmp/mcp_fifo; TAURI_MCP_IPC_PATH="$MCP" npx tauri-plugin-mcp-server < /tmp/mcp_fifo > "$1" 2>/dev/null & pid=$!; sleep 1; echo "$2" > /tmp/mcp_fifo; sleep "$3"; exec 3>/tmp/mcp_fifo 2>/dev/null; exec 3>&-; kill $pid 2>/dev/null; wait $pid 2>/dev/null; }
ck() { if python3 -c "import json; d=json.load(open('$1')); assert 'result' in d and not d.get('isError', False), str(d.get('error', d))" 2>/dev/null; then echo "  ✅ $2"; PASS=$((PASS+1)); else echo "  ❌ $2"; FAIL=$((FAIL+1)); fi }
ck_js() { if python3 -c "import json; d=json.load(open('$1'));
for item in d['result']['content']:
    if item.get('type') == 'text' and 'true' in item['text'].lower():
        exit(0)
exit(1)" 2>/dev/null; then echo "  ✅ $2"; PASS=$((PASS+1)); else echo "  ❌ $2"; FAIL=$((FAIL+1)); fi }

echo ""; echo "============================================================"; echo "  Link-Searcher 全量 E2E 测试"; echo "============================================================"; echo ""

echo "=== [1] 搜索页 ==="
mcp /tmp/t1.json "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"navigate\",\"arguments\":{\"action\":\"goto\",\"url\":\"http://localhost:1420/#/\"}}}" 4
mcp /tmp/t2.json "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"query_page\",\"arguments\":{\"mode\":\"state\"}}}" 4
ck /tmp/t2.json "1.1 搜索页加载"
mcp /tmp/t3.json "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"execute_js\",\"arguments\":{\"code\":\"document.body.innerText.includes('搜索您的文档')\"}}}" 5
ck_js /tmp/t3.json "1.2 搜索页内容已渲染"
mcp /tmp/t4.json "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"execute_js\",\"arguments\":{\"code\":\"document.body.innerText.includes('语义')\"}}}" 5
ck_js /tmp/t4.json "1.3 语义搜索入口"

echo "=== [2] 浏览页 ==="
mcp /tmp/t5.json "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"navigate\",\"arguments\":{\"action\":\"goto\",\"url\":\"http://localhost:1420/#/browse\"}}}" 4
mcp /tmp/t6.json "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"tools/call\",\"params\":{\"name\":\"query_page\",\"arguments\":{\"mode\":\"state\"}}}" 4
ck /tmp/t6.json "2.1 浏览页加载"

echo "=== [3] 资料库页 ==="
mcp /tmp/t7.json "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{\"name\":\"navigate\",\"arguments\":{\"action\":\"goto\",\"url\":\"http://localhost:1420/#/directories\"}}}" 4
mcp /tmp/t8.json "{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"tools/call\",\"params\":{\"name\":\"query_page\",\"arguments\":{\"mode\":\"state\"}}}" 4
ck /tmp/t8.json "3.1 资料库页加载"

echo "=== [4] 索引状态页 ==="
mcp /tmp/t9.json "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/call\",\"params\":{\"name\":\"navigate\",\"arguments\":{\"action\":\"goto\",\"url\":\"http://localhost:1420/#/index\"}}}" 4
mcp /tmp/t10.json "{\"jsonrpc\":\"2.0\",\"id\":10,\"method\":\"tools/call\",\"params\":{\"name\":\"query_page\",\"arguments\":{\"mode\":\"state\"}}}" 4
ck /tmp/t10.json "4.1 索引状态页加载"
mcp /tmp/t11.json "{\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"tools/call\",\"params\":{\"name\":\"execute_js\",\"arguments\":{\"code\":\"document.body.innerText.includes('已索引')\"}}}" 5
ck_js /tmp/t11.json "4.2 索引统计"
mcp /tmp/t12.json "{\"jsonrpc\":\"2.0\",\"id\":12,\"method\":\"tools/call\",\"params\":{\"name\":\"execute_js\",\"arguments\":{\"code\":\"document.body.innerText.includes('开始扫描')\"}}}" 5
ck_js /tmp/t12.json "4.3 扫描按钮"

echo "=== [5] 文件类型页 ==="
mcp /tmp/t13.json "{\"jsonrpc\":\"2.0\",\"id\":13,\"method\":\"tools/call\",\"params\":{\"name\":\"navigate\",\"arguments\":{\"action\":\"goto\",\"url\":\"http://localhost:1420/#/file-types\"}}}" 4
mcp /tmp/t14.json "{\"jsonrpc\":\"2.0\",\"id\":14,\"method\":\"tools/call\",\"params\":{\"name\":\"query_page\",\"arguments\":{\"mode\":\"state\"}}}" 4
ck /tmp/t14.json "5.1 文件类型页加载"

echo "=== [6] 设置页 ==="
mcp /tmp/t15.json "{\"jsonrpc\":\"2.0\",\"id\":15,\"method\":\"tools/call\",\"params\":{\"name\":\"navigate\",\"arguments\":{\"action\":\"goto\",\"url\":\"http://localhost:1420/#/settings\"}}}" 4
mcp /tmp/t16.json "{\"jsonrpc\":\"2.0\",\"id\":16,\"method\":\"tools/call\",\"params\":{\"name\":\"query_page\",\"arguments\":{\"mode\":\"state\"}}}" 4
ck /tmp/t16.json "6.1 设置页加载"
mcp /tmp/t17.json "{\"jsonrpc\":\"2.0\",\"id\":17,\"method\":\"tools/call\",\"params\":{\"name\":\"execute_js\",\"arguments\":{\"code\":\"document.body.innerText.includes('OCR')\"}}}" 5
ck_js /tmp/t17.json "6.2 OCR 设置项"

echo "=== [7] 日志页 ==="
mcp /tmp/t18.json "{\"jsonrpc\":\"2.0\",\"id\":18,\"method\":\"tools/call\",\"params\":{\"name\":\"navigate\",\"arguments\":{\"action\":\"goto\",\"url\":\"http://localhost:1420/#/logs\"}}}" 4
mcp /tmp/t19.json "{\"jsonrpc\":\"2.0\",\"id\":19,\"method\":\"tools/call\",\"params\":{\"name\":\"query_page\",\"arguments\":{\"mode\":\"state\"}}}" 4
ck /tmp/t19.json "7.1 日志页加载"

echo "=== [8] 主题切换 ==="
mcp /tmp/t20.json "{\"jsonrpc\":\"2.0\",\"id\":20,\"method\":\"tools/call\",\"params\":{\"name\":\"click\",\"arguments\":{\"selector_type\":\"text\",\"selector_value\":\"深色\",\"match\":\"contains\"}}}" 4
ck /tmp/t20.json "8.1 主题按钮点击"

echo "=== [9] AI 聊天 ==="
mcp /tmp/t21.json "{\"jsonrpc\":\"2.0\",\"id\":21,\"method\":\"tools/call\",\"params\":{\"name\":\"navigate\",\"arguments\":{\"action\":\"goto\",\"url\":\"http://localhost:1420/#/chat\"}}}" 4
mcp /tmp/t22.json "{\"jsonrpc\":\"2.0\",\"id\":22,\"method\":\"tools/call\",\"params\":{\"name\":\"query_page\",\"arguments\":{\"mode\":\"state\"}}}" 4
ck /tmp/t22.json "9.1 AI 聊天页加载"
mcp /tmp/t23.json "{\"jsonrpc\":\"2.0\",\"id\":23,\"method\":\"tools/call\",\"params\":{\"name\":\"execute_js\",\"arguments\":{\"code\":\"document.querySelector('input[placeholder*=\\\"追问\\\"]') !== null\"}}}" 5
ck_js /tmp/t23.json "9.2 聊天输入框"
mcp /tmp/t24.json "{\"jsonrpc\":\"2.0\",\"id\":24,\"method\":\"tools/call\",\"params\":{\"name\":\"type_text\",\"arguments\":{\"text\":\"2025年营收增长了多少\",\"selector_type\":\"css\",\"selector_value\":\"input[placeholder*='追问']\"}}}" 4
ck /tmp/t24.json "9.3 输入问题"
mcp /tmp/t25.json "{\"jsonrpc\":\"2.0\",\"id\":25,\"method\":\"tools/call\",\"params\":{\"name\":\"click\",\"arguments\":{\"selector_type\":\"text\",\"selector_value\":\"发送\",\"match\":\"exact\"}}}" 4
ck /tmp/t25.json "9.4 发送按钮"
echo "  等待 AI 响应..."
FOUND=false
for i in $(seq 1 15); do
  sleep 2
  mcp /tmp/t26.json "{\"jsonrpc\":\"2.0\",\"id\":26,\"method\":\"tools/call\",\"params\":{\"name\":\"execute_js\",\"arguments\":{\"code\":\"!document.body.innerText.includes('思考中')\"}}}" 5
  if python3 -c "import json; d=json.load(open('/tmp/t26.json')); 
for item in d['result']['content']:
    if item.get('type') == 'text' and 'true' in item['text'].lower():
        exit(0)
exit(1)" 2>/dev/null; then
    FOUND=true; echo "  ✅ 9.5 AI 响应完成（约 $((i*2)) 秒）"; PASS=$((PASS+1)); break
  fi
done
[ "$FOUND" = false ] && echo "  ❌ 9.5 AI 响应超时" && FAIL=$((FAIL+1))
mcp /tmp/t27.json "{\"jsonrpc\":\"2.0\",\"id\":27,\"method\":\"tools/call\",\"params\":{\"name\":\"execute_js\",\"arguments\":{\"code\":\"document.body.innerText.includes('营收')\"}}}" 5
ck_js /tmp/t27.json "9.6 AI 回答包含营收"
mcp /tmp/t28.json "{\"jsonrpc\":\"2.0\",\"id\":28,\"method\":\"tools/call\",\"params\":{\"name\":\"execute_js\",\"arguments\":{\"code\":\"document.body.innerText.includes('⏱')\"}}}" 5
ck_js /tmp/t28.json "9.7 显示耗时"
mcp /tmp/t29.json "{\"jsonrpc\":\"2.0\",\"id\":29,\"method\":\"tools/call\",\"params\":{\"name\":\"click\",\"arguments\":{\"selector_type\":\"text\",\"selector_value\":\"检索依据\",\"match\":\"contains\"}}}" 4
ck /tmp/t29.json "9.8 展开检索依据"
mcp /tmp/t30.json "{\"jsonrpc\":\"2.0\",\"id\":30,\"method\":\"tools/call\",\"params\":{\"name\":\"execute_js\",\"arguments\":{\"code\":\"document.body.innerText.includes('检索依据')\"}}}" 5
ck_js /tmp/t30.json "9.9 证据面板"

echo ""; echo "=============================="; echo "  结果: $PASS 通过, $FAIL 失败"; echo "=============================="
exit $FAIL
