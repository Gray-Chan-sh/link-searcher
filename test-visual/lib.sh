#!/bin/bash
# ==============================================================================
# Link-Searcher GUI 测试工具库 (lib.sh)
# 说明: 本文件被各用例脚本 source。提供 MCP 通信 / 截图 / JS 断言 / 记录 工具。
# 用法: source "$TEST_VISUAL_DIR/lib.sh"
# ==============================================================================

# ── 环境变量默认值 ──
# MCP socket 路径; 自动探测 if 未设置
# 可由 run-all.sh 统一设置
# MCP_SOCK: Tauri MCP socket
# TESTOUT:  测试输出根目录 (current)

set -u                              # 未定义变量报错 (不 set -e, 断言失败不应中断脚本)
TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-}"
if [ -z "$TEST_VISUAL_DIR" ]; then
    # 支持两种调用: 直接执行 (BASH_SOURCE 生效) / source 时用 $PWD 推断
    if [ -n "${BASH_SOURCE[0]:-}" ]; then
        TEST_VISUAL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    else
        TEST_VISUAL_DIR="$(cd "$(dirname "$0")" && pwd)"
    fi
fi

# ── MCP socket 探测 ──
detect_mcp_socket() {
    if [ -n "${MCP_SOCK:-}" ] && [ -S "$MCP_SOCK" ]; then
        echo "$MCP_SOCK"
        return 0
    fi
    local cand
    cand="$(find /var/folders -name "tauri-mcp.sock" -type s 2>/dev/null | head -1)"
    if [ -n "$cand" ]; then echo "$cand"; return 0; fi
    return 1
}

# 确保 MCP_SOCK 已设置; 找不到则报错退出
require_mcp() {
    if [ -z "${MCP_SOCK:-}" ]; then
        MCP_SOCK="$(detect_mcp_socket)" || {
            echo "❌ 找不到 MCP socket。请先运行: npx tauri dev" >&2
            return 1
        }
        echo "✅ MCP socket: $MCP_SOCK"
    fi
}

# ── MCP 调用 (单次 bridge + 自动重启 App) ──
MCP_BRIDGE="$TEST_VISUAL_DIR/mcp_bridge.py"
MCP_FAIL_COUNT=0

restart_app() {
    echo "  🔄 MCP 失效, 重启 App..."
    pkill -f "target/debug/link-searcher" 2>/dev/null
    pkill -f "vite" 2>/dev/null
    pkill -f "tauri dev" 2>/dev/null
    sleep 3
    rm -f /var/folders/*/*/T/tauri-mcp.sock* 2>/dev/null
    cd "$TEST_VISUAL_DIR/.." && nohup npx tauri dev > /tmp/ls-tauri-dev.log 2>&1 & disown
    cd "$TEST_VISUAL_DIR/.."
    for i in $(seq 1 30); do
        PS=$(ps aux | grep "target/debug/link-searcher" | grep -v grep | head -1)
        S=$(find /var/folders -name "tauri-mcp.sock" -type s 2>/dev/null | head -1)
        if [ -n "$PS" ] && [ -n "$S" ]; then
            MCP_SOCK="$S"
            sleep 3
            echo "  ✅ App 重启完成 $(date +%T)"
            return 0
        fi
        sleep 10
    done
    echo "  ❌ App 重启失败"
    return 1
}

mcp() {
    local tool="$1"
    local args_json="${2:-}"
    local timeout="${3:-8}"
    require_mcp || return 1
    local req resp
    req="{\"tool\":\"$tool\",\"args\":$args_json,\"timeout\":$timeout}"
    resp="$(MCP_SOCK="$MCP_SOCK" python3 "$MCP_BRIDGE" <<< "$req" 2>/dev/null)"

    # 检测 MCP 失效: 任意 timeout/Timeout 关键词触发 App 重启重试一次
    if printf '%s' "$resp" | grep -qi '"\(timeout\|Timeout\)"' || \
       printf '%s' "$resp" | grep -qi 'Timeout waiting for' || \
       [ -z "$resp" ]; then
        MCP_FAIL_COUNT=$((MCP_FAIL_COUNT + 1))
        if [ "$MCP_FAIL_COUNT" -ge 2 ]; then
            restart_app || return 1
            MCP_FAIL_COUNT=0
            resp="$(MCP_SOCK="$MCP_SOCK" python3 "$MCP_BRIDGE" <<< "$req" 2>/dev/null)"
        fi
    else
        MCP_FAIL_COUNT=0
    fi
    echo "$resp"
}

# ── JS 执行 → 返回纯文本值 (取第一行) ──
js() {
    local code="$1"
    local code_json
    code_json="$(python3 -c 'import json,sys; print(json.dumps({"code": sys.argv[1]}))' "$code")"
    local resp
    resp="$(mcp "execute_js" "$code_json" 8)"
    echo "$resp" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    if not d.get("ok"): sys.exit(0)
    print(d.get("text", "").split("\n")[0])
except Exception:
    pass
'
}

# ── JSON 字符串转义 ──
jq_escape() {
    python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))' <<< "$1"
}

# ── 断言: JS 表达式结果 == 期望 ├──
# 用法: assert_js 'document.querySelector(".x") !== null' 'true' "元素存在"
# 通过: 打印 ✅ PASS; 失败: 打印 ❌ FAIL + 记录到当前用例结果
CASE_ID_CUR=""       # 当前用例 ID (由用例脚本设置)
CASE_DIR_CUR=""      # 当前用例截图目录
STEP_N=0             # 当前步骤号
ASSERT_FAILS_CUR=0   # 本用例失败断言数
CURRENT_CASE_NAME=""

# 记录当前用例上下文 (由用例脚本在开头调用)
case_begin() {
    CASE_ID_CUR="$1"
    CURRENT_CASE_NAME="$2"
    local id_lower dc
    id_lower="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
    dc="${TESTOUT:-$TEST_VISUAL_DIR/current}"
    CASE_DIR_CUR="$dc/$id_lower"
    mkdir -p "$CASE_DIR_CUR"
    STEP_N=0
    ASSERT_FAILS_CUR=0
}

# 截图: 保存当前 webview 画面
# 用法: snap "步骤描述"
SNAP_DIR="$HOME/ls-shots"   # take_screenshot 只允许 home 或系统 temp

snap() {
    local desc="${1:-}"
    STEP_N=$((STEP_N + 1))
    local fname="step-${STEP_N}.png"
    mkdir -p "$SNAP_DIR"

    local raw
    raw="$(mcp "take_screenshot" "{\"output_dir\":\"$SNAP_DIR\"}" 15)"
    local saved=""
    saved="$(printf '%s' "$raw" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    print(d.get("text", ""))
except Exception:
    print("")
' 2>/dev/null | grep -o 'Full screenshot saved to: *[^ ]*\.\(jpg\|jpeg\|png\)' | head -1 | sed 's/Full screenshot saved to: *//')"

    if [ -n "$saved" ] && [ -f "$saved" ]; then
        if command -v sips >/dev/null 2>&1; then
            sips -s format png "$saved" --out "$CASE_DIR_CUR/$fname" >/dev/null 2>&1
        else
            cp "$saved" "$CASE_DIR_CUR/$fname"
        fi
        rm -f "$saved"
        echo "  📸 $fname: $desc"
    else
        echo "  ⚠️  截图失败: $desc ()"
    fi
}

# JS 断言
# 用法: expect_js 'JS 表达式' '期望值' '描述'
# 判定: JS 返回值 == 期望值 → PASS, 否则 FAIL
expect_js() {
    local expr="$1" expected="$2" desc="$3"
    STEP_N=$((STEP_N + 1))
    local got
    got="$(js "$expr")"
    got="$(printf '%s' "$got" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
    if [ "$got" = "$expected" ]; then
        echo "  ✅ Step $STEP_N [$desc]: got '$got' == expect '$expected'"
        return 0
    else
        echo "  ❌ Step $STEP_N [$desc]: got '$got' != expect '$expected'"
        ASSERT_FAILS_CUR=$((ASSERT_FAILS_CUR + 1))
        return 1
    fi
}

# 断言: JS 返回的数值 > 0
# 用法: expect_count 'document.querySelectorAll(".result-item").length' "结果数"
expect_count() {
    local expr="$1" desc="$2"
    STEP_N=$((STEP_N + 1))
    local got
    got="$(js "$expr")"
    got="$(printf '%s' "$got" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
    if [ -n "$got" ] && [ "$got" -gt 0 ] 2>/dev/null; then
        echo "  ✅ Step $STEP_N [$desc]: count=$got > 0"
        return 0
    else
        echo "  ❌ Step $STEP_N [$desc]: count='$got' not > 0"
        ASSERT_FAILS_CUR=$((ASSERT_FAILS_CUR + 1))
        return 1
    fi
}

# 用例结束: 返回 0=通过 非0=失败
case_end() {
    if [ "$ASSERT_FAILS_CUR" -eq 0 ]; then
        record_case "$CASE_ID_CUR" "$CURRENT_CASE_NAME" "PASS"
        echo "  🎉 用例通过"
        return 0
    else
        record_case "$CASE_ID_CUR" "$CURRENT_CASE_NAME" "FAIL"
        echo "  💥 用例失败 ($ASSERT_FAILS_CUR 处断言失败)"
        return 1
    fi
}

# ── 记录结果到 results.tsv ──
# 列: id|name|status|timestamp|screenshot_dir
RESULTS_FILE="${RESULTS_FILE:-$TEST_VISUAL_DIR/results.tsv}"
record_case() {
    local id="$1" name="$2" status="$3"
    mkdir -p "$(dirname "$RESULTS_FILE")"
    printf '%s|%s|%s|%s|%s\n' \
        "$id" "$name" "$status" "$(date -Iseconds)" "$CASE_DIR_CUR" \
        >> "$RESULTS_FILE"
}

# ── 导航 helper (用 hash, 不触发 webview reload) ──
goto() {
    js "window.location.hash = \"$1\"; true" >/dev/null
    sleep 1
}

# ── 固定窗口大小 ──
resize_window() {
    mcp "manage_window" '{"action":"set_size","width":1280,"height":800}' 5 >/dev/null
}

# ── React 兼容点击 (dispatch MouseEvent, React 17+ 需要) ──
click() {
    local sel="$1"
    js "
var el=document.querySelector(\"$sel\");
if(!el) return \"not found\";
el.dispatchEvent(new MouseEvent(\"click\",{bubbles:true,cancelable:true,composed:true}));
\"clicked\"
" >/dev/null
}
clickid() { click "#$1"; }
clickcss() { click "$1"; }
clicktext() {
    local txt="$1"
    js "
var els=Array.from(document.querySelectorAll(\"button,a,[role=button]\"));
var el=els.find(e=>e.textContent.includes(\"$txt\"));
if(!el) return \"not found\";
el.dispatchEvent(new MouseEvent(\"click\",{bubbles:true,cancelable:true,composed:true}));
\"clicked\"
" >/dev/null
}

# ── React 兼容输入 (原生 value setter, React 17+ 需要) ──
type() {
    local text="$1" selector="${2:-input[data-search-input=true]}"
    js "
var el=document.querySelector(\"$selector\");
if(!el) return \"not found\";
var nativeSetter=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,\"value\").set;
nativeSetter.call(el,\"$text\");
el.dispatchEvent(new Event(\"input\",{bubbles:true}));
el.dispatchEvent(new Event(\"change\",{bubbles:true}));
\"typed\"
" >/dev/null
}

# ── 按键 (原生 KeyboardEvent) ──
press() {
    js "
var el=document.activeElement || document.querySelector(\"input[data-search-input=true]\");
el.dispatchEvent(new KeyboardEvent(\"keydown\",{key:\"$1\",code:\"$1\",keyCode:13,bubbles:true,cancelable:true}));
el.dispatchEvent(new KeyboardEvent(\"keyup\",{key:\"$1\",code:\"$1\",keyCode:13,bubbles:true}));
\"pressed\"
" >/dev/null
}

# ── 等待条件 (轮询直至超时) ──
# 用法: wait_js 'JS 表达式 truthy' '超时秒' '描述'
wait_js() {
    local expr="$1" timeout="${2:-10}" desc="${3:-等待条件}"
    local deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        local got
        got="$(js "$expr")"
        got="$(printf '%s' "$got" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
        if [ "$got" = "true" ] || { [ -n "$got" ] && [ "$got" -gt 0 ] 2>/dev/null; }; then
            echo "  ✅ wait [$desc]: 满足 (got '$got')"
            return 0
        fi
        sleep 1
    done
    echo "  ❌ wait [$desc]: 超时 ${timeout}s"
    ASSERT_FAILS_CUR=$((ASSERT_FAILS_CUR + 1))
    return 1
}

# ── 输出用例标题 ──
section() {
    echo ""
    echo "======================================================================"
    echo "  [$CASE_ID_CUR] $CURRENT_CASE_NAME"
    echo "======================================================================"
}
