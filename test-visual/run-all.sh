#!/bin/bash
# ==============================================================================
# Link-Searcher GUI 测试主控 (run-all.sh)
# 用法:
#   ./test-visual/run-all.sh              # 跑全部用例
#   ./test-visual/run-all.sh --p0         # 只跑 P0
#   ./test-visual/run-all.sh --section search  # 只跑搜索页用例
#   ./test-visual/run-all.sh --update-baseline # 更新基线
#   ./test-visual/run-all.sh --case G-11  # 只跑单个用例
# ==============================================================================

set -euo pipefail

TEST_VISUAL_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$TEST_VISUAL_DIR/lib.sh"

# 默认环境
export TESTOUT="${TEST_VISUAL_DIR}/current"
export RESULTS_FILE="${TEST_VISUAL_DIR}/results.tsv"

# 统计
TOTAL=0; PASSED=0; FAILED=0; SKIPPED=0

# 标记: 是否更新基线
UPDATE_BASELINE=false

# 过滤参数
FILTER_SECTION=""       # 分区名 (search, browse, index, ...)
FILTER_P0=false
FILTER_CASE=""          # 单个用例 ID

# 解析参数
while [ $# -gt 0 ]; do
    case "$1" in
        --p0)         FILTER_P0=true ;;
        --section)    FILTER_SECTION="$2"; shift ;;
        --case)       FILTER_CASE="$2"; shift ;;
        --update-baseline) UPDATE_BASELINE=true ;;
        --help|-h)
            echo "用法: $0 [--p0] [--section <name>] [--case <id>] [--update-baseline]"
            echo "  --p0          只跑 P0 用例"
            echo "  --section     只跑指定分区 (search/browse/index/settings/ai-chat/...)"
            echo "  --case        只跑单个用例 (G-01, G-11, ...)"
            echo "  --update-baseline  更新 baseline 截图"
            exit 0
            ;;
        *) echo "未知参数: $1"; exit 1 ;;
    esac
    shift
done

# 清理上次结果
rm -f "$RESULTS_FILE"
rm -rf "$TESTOUT"
mkdir -p "$TESTOUT"

# 检测 MCP socket
MCP_SOCK="$(detect_mcp_socket)" || {
    echo "❌ 找不到 MCP socket。请先启动: npx tauri dev"
    exit 1
}
export MCP_SOCK
echo "✅ MCP socket: $MCP_SOCK"

# 固定窗口大小
resize_window
echo "✅ 窗口大小: 1280x800"

# ── 用例自动发现 ──
# 从 cases/*.sh 扫描, 按文件名 ID 排序 (G-01, G-02, ...)
CASE_FILES=()
for cf in $(ls "$TEST_VISUAL_DIR"/cases/*.sh 2>/dev/null | sort -t- -k2 -n); do
    CASE_FILES+=("$cf")
done

# 元数据: ID|名称|优先级 (平行数组, bash 3.2 兼容)
META_IDS=()
META_NAMES=()
META_PRIOS=()

# 元数据表: ID|名称|优先级 (脚本命名为 G-XX.sh, 元数据用于过滤与展示)
META=(
    "G-01|App启动→窗口出现|P0"
    "G-02|侧边栏8个导航项|P0"
    "G-04|状态栏显示索引计数|P0"
    "G-10|空搜索→不崩溃|P0"
    "G-11|关键词搜索→结果出现|P0"
    "G-12|结果渲染|P1"
    "G-13|结果显示文件信息|P1"
    "G-14|中文关键词搜索|P0"
    "G-15|英文关键词搜索|P0"
    "G-16|中英混合搜索|P1"
    "G-17|无结果→引导显示|P0"
    "G-18|模糊搜索|P1"
    "G-19|通配符doc*|P1"
    "G-21|文件名搜索|P1"
    "G-22|搜索建议|P1"
    "G-23|建议键盘导航|P1"
    "G-24|排序切换|P1"
    "G-26|分页跳页|P1"
    "G-27|新查询回第1页|P1"
    "G-28|目录树筛选|P1"
    "G-29|扩展名筛选|P1"
    "G-30|清空筛选|P1"
    "G-32|CSV导出|P1"
    "G-33|语义搜索开关|P1"
    "G-40|浏览页文件列表|P0"
    "G-41|浏览页状态筛选|P0"
    "G-42|浏览筛选未索引|P1"
    "G-43|浏览筛选失败|P1"
    "G-44|浏览类型筛选|P1"
    "G-45|浏览文件名搜索|P1"
    "G-46|浏览排序按大小|P1"
    "G-47|浏览排序按时间|P1"
    "G-49|分页→下一页|P0"
    "G-60|点击结果→预览出现|P0"
    "G-61|文本预览→内容正确|P0"
    "G-62|图片预览→缩放|P1"
    "G-63|PDF预览|P1"
    "G-70|统计卡片显示|P0"
    "G-73|点击扫描→进度条|P0"
    "G-79|重建索引确认|P0"
    "G-90|主题切换|P0"
    "G-92|主题跟随系统|P1"
    "G-93|语言切换→英文|P0"
    "G-94|语言切换→日文|P1"
    "G-95|语言切换→韩文|P1"
    "G-96|语言切换→中文|P0"
    "G-97|设置改值→自动保存|P0"
    "G-98|OCR引擎切换|P1"
    "G-99|OCR语言切换|P1"
    "G-100|OCR引擎测试|P1"
    "G-101|语义权重滑杆|P1"
    "G-102|添加AI Provider|P1"
    "G-107|未配AI→语义隐藏|P0"
    "G-120|聊天页加载输入框|P0"
    "G-121|新建会话|P1"
    "G-124|回答含引用|P1"
    "G-125|引用跳转浏览|P1"
    "G-126|检索依据面板|P1"
    "G-127|推理过程时间线|P1"
    "G-128|@引用文件|P0"
    "G-129|范围chip删除|P1"
    "G-130|取消请求|P1"
    "G-132|会话删除|P1"
    "G-133|追问换范围|P1"
    "G-140|目录列表加载|P0"
    "G-141|添加目录|P0"
    "G-142|编辑目录别名|P1"
    "G-143|删除目录|P0"
    "G-150|日志列表加载|P0"
    "G-151|日志类型筛选OCR|P1"
    "G-152|日志关键字过滤|P1"
    "G-154|会话日志切换|P1"
    "G-160|类型列表加载|P0"
    "G-161|缺依赖格式显示|P1"
    "G-162|不支持扩展名展示|P1"
    "G-170|索引计数显示|P0"
    "G-172|任务简报|P1"
    "G-180|搜索→缩小范围|P0"
    "G-181|索引统计跳转浏览|P1"
    "G-182|浏览多选AI问答|P1"
    "G-183|聊天引用跳转|P1"
    "G-184|任务简报跳转日志|P1"
    "G-185|搜索键盘导航|P1"
    "G-190|CmdK聚焦搜索框|P1"
    "G-191|上下结果选择|P1"
    "G-192|Enter打开预览|P1"
    "G-200|触发备份|P1"
    "G-20|短语搜索|P2"
    "G-31|筛选刷新后保持|P2"
    "G-48|排序方向切换|P2"
    "G-56|列宽拖拽|P2"
    "G-66|预览复制路径|P2"
    "G-67|预览Finder显示|P2"
    "G-108|排除规则编辑|P2"
    "G-109|Web API开关|P2"
    "G-131|会话导出|P2"
    "G-144|拖拽添加目录|P2"
    "G-153|清除日志|P2"
    "G-201|ZIP导出加密|P2"
    "G-202|ZIP恢复|P2"
    "G-203|死目录检测重映射|P2"
)
for m in "${META[@]}"; do
    IFS='|' read -r mid mname mprio <<< "$m"
    META_IDS+=("$mid")
    META_NAMES+=("$mname")
    META_PRIOS+=("$mprio")
done

# 从元数据查找 ID → 返回 (名称, 优先级) 输出到全局
find_meta() {
    local target="$1" i
    for i in $(seq 0 $(( ${#META_IDS[@]} - 1 ))); do
        if [ "${META_IDS[$i]}" = "$target" ]; then
            META_NAME="${META_NAMES[$i]}"
            META_PRIO="${META_PRIOS[$i]}"
            return 0
        fi
    done
    META_NAME="$target"
    META_PRIO="P1"
    return 1
}

# ── 执行单个用例 ──
run_case() {
    local id="$1" name="$2" priority="$3"
    TOTAL=$((TOTAL + 1))

    local script_path="$TEST_VISUAL_DIR/cases/${id}.sh"

    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  [$id] $name ($priority)"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

    if bash "$script_path"; then
        PASSED=$((PASSED + 1))
    else
        FAILED=$((FAILED + 1))
    fi
}

# ── 主循环 ──
echo "══════════════════════════════════════════════════════════════════════"
echo "  Link-Searcher GUI 测试"
echo "  $(date '+%Y-%m-%d %H:%M:%S')"
echo "══════════════════════════════════════════════════════════════════════"

for case_file in "${CASE_FILES[@]}"; do
    id="$(basename "$case_file" .sh)"
    find_meta "$id"
    name="$META_NAME"
    priority="$META_PRIO"

    # 过滤
    [ -n "$FILTER_CASE" ] && [ "$id" != "$FILTER_CASE" ] && continue
    [ "$FILTER_P0" = true ] && [ "$priority" != "P0" ] && continue

    run_case "$id" "$name" "$priority"
done

# ── 汇总 ──
echo ""
echo "══════════════════════════════════════════════════════════════════════"
echo "  测试完成"
echo "══════════════════════════════════════════════════════════════════════"
echo "  总计: $TOTAL | 通过: $PASSED | 失败: $FAILED | 跳过: $SKIPPED"
echo ""

# 生成报告
if [ -f "$TEST_VISUAL_DIR/generate-report.sh" ]; then
    bash "$TEST_VISUAL_DIR/generate-report.sh"
fi

# 更新基线
if [ "$UPDATE_BASELINE" = true ]; then
    echo "  ↑ 更新 baseline..."
    rm -rf "$TEST_VISUAL_DIR/baseline"
    cp -r "$TESTOUT" "$TEST_VISUAL_DIR/baseline"
    echo "  ✅ baseline 已更新"
fi

# 退出码
[ "$FAILED" -eq 0 ] && exit 0 || exit 1