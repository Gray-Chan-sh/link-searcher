#!/bin/bash
# Link-Searcher 性能测试脚本
# 用法: ./perf_scan.sh <test_data_dir> [file_count_label]
#
# 示例:
#   ./perf_scan.sh /tmp/ls-test-1k
#   ./perf_scan.sh ~/Documents "docs-5k"
set -e

DATA_DIR="${1:?Usage: $0 <test_data_dir> [file_count_label]}"
LABEL="${2:-$(basename "$DATA_DIR")}"
APP_DATA="/tmp/ls-perf-data"
MEM_LOG="/tmp/ls-perf-mem.log"
REPORT_DIR=".omo/reports"
REPORT_LABEL="$(echo "$LABEL" | tr '/ ' '__')"
REPORT_FILE="$REPORT_DIR/perf-$REPORT_LABEL-$(date +%Y%m%d-%H%M%S).md"
BIN_PATH="$(pwd)/src-tauri/target/release/link-searcher"

# 校验测试目录
if [ ! -d "$DATA_DIR" ]; then
    echo "错误: 测试目录不存在: $DATA_DIR"
    exit 1
fi

# 固定使用 release 二进制，避免命中 PATH 中的旧版本
if [ ! -f "$BIN_PATH" ]; then
    echo "错误: 未找到 release 二进制: $BIN_PATH"
    echo "请先在仓库根目录执行: npm run tauri build"
    exit 1
fi

mkdir -p "$REPORT_DIR"

echo "=== Link-Searcher 性能测试 ==="
echo "数据目录: $DATA_DIR"
echo "标签:     $LABEL"
echo "应用数据: $APP_DATA"
echo "二进制:   $BIN_PATH"

# ─── 清理 ──────────────────────────────────────────────────
rm -rf "$APP_DATA"
mkdir -p "$APP_DATA"

# ─── 统计输入数据 ──────────────────────────────────────────
FILE_COUNT=$(find "$DATA_DIR" -type f | wc -l | tr -d ' ')
TOTAL_SIZE=$(du -sh "$DATA_DIR" 2>/dev/null | cut -f1)
echo "文件数:   $FILE_COUNT"
echo "总大小:   $TOTAL_SIZE"

# ─── 启动应用（后台）──────────────────────────────────────
echo ""
echo "[1/3] 启动 link-searcher（后台）..."

"$BIN_PATH" --data-dir "$APP_DATA" &
APP_PID=$!
echo "PID:      $APP_PID"

# 等待应用就绪并校验 --data-dir 参数生效（进程存活）
echo "等待应用就绪..."
sleep 2
if ! kill -0 "$APP_PID" 2>/dev/null; then
    echo "错误: 应用启动失败（进程已退出），--data-dir 参数可能不被支持"
    wait "$APP_PID" 2>/dev/null || true
    exit 1
fi

# ─── 监控内存 ──────────────────────────────────────────────
echo ""
echo "[2/3] 监控内存中（每 5 秒）..."
echo "时间 RSS(KB)" > "$MEM_LOG"
MEM_POLL_PID=""

# macOS: ps -o rss= -p <pid>  ; Linux: 相同
poll_mem() {
    while true; do
        if kill -0 "$APP_PID" 2>/dev/null; then
            RSS=$(ps -o rss= -p "$APP_PID" 2>/dev/null | tr -d ' ' || echo "0")
            echo "$(date +%H:%M:%S) $RSS" >> "$MEM_LOG"
        fi
        sleep 5
    done
}
poll_mem &
MEM_POLL_PID=$!

# ─── 触发扫描 ──────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "请手动操作："
echo "  1. 在 Link-Searcher 中添加目录: $DATA_DIR"
echo "  2. 点击「索引状态」→ 「全量扫描」"
echo "  3. 等待扫描完成"
echo "  4. 按 Enter 键继续报告"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
read

# ─── 停止监控 ──────────────────────────────────────────────
kill "$MEM_POLL_PID" 2>/dev/null || true
wait "$MEM_POLL_PID" 2>/dev/null || true

# 优雅停止应用
kill "$APP_PID" 2>/dev/null || true
wait "$APP_PID" 2>/dev/null || true

# ─── 计算统计 ──────────────────────────────────────────────
INDEX_SIZE="N/A"
if [ -d "$APP_DATA/.ls-index" ]; then
    INDEX_SIZE=$(du -sh "$APP_DATA/.ls-index" 2>/dev/null | cut -f1)
fi
DB_SIZE="N/A"
if [ -f "$APP_DATA/data.db" ]; then
    DB_SIZE=$(du -sh "$APP_DATA/data.db" 2>/dev/null | cut -f1)
fi

PEAK_RSS=0
AVG_MB=0
SAMPLES=0
if [ -s "$MEM_LOG" ]; then
    PEAK_RSS=$(awk 'NR>1 {print $2}' "$MEM_LOG" | sort -nr | head -1)
    PEAK_RSS=${PEAK_RSS:-0}
    SAMPLES=$(awk 'NR>1' "$MEM_LOG" | wc -l | tr -d ' ')
fi
PEAK_MB=$(awk "BEGIN {printf \"%.1f\", $PEAK_RSS / 1024}")
if [ "$SAMPLES" -gt 0 ]; then
    AVG_RSS=$(awk 'NR>1 {sum+=$2; n++} END {printf "%.0f", sum/n}' "$MEM_LOG")
    AVG_MB=$(awk "BEGIN {printf \"%.1f\", $AVG_RSS / 1024}")
fi

# ─── 生成 Markdown 报表 ────────────────────────────────────
{
    echo "# Link-Searcher 性能测试报告"
    echo ""
    echo "- 日期: $(date '+%Y-%m-%d %H:%M:%S')"
    echo "- 数据目录: $DATA_DIR"
    echo "- 标签: $LABEL"
    echo "- 二进制: $BIN_PATH（release）"
    echo ""
    echo "## 输入数据"
    echo "| 指标 | 值 |"
    echo "|------|-----|"
    echo "| 文件数 | $FILE_COUNT |"
    echo "| 总大小 | $TOTAL_SIZE |"
    echo ""
    echo "## 扫描结果"
    echo "| 指标 | 值 |"
    echo "|------|-----|"
    echo "| 索引大小 | $INDEX_SIZE |"
    echo "| DB 大小 | $DB_SIZE |"
    echo "| 内存峰值 | $PEAK_MB MB |"
    echo "| 内存均值 | $AVG_MB MB |"
    echo "| 采样次数 | $SAMPLES |"
    echo ""
    echo "## 内存采样"
    echo "| 时间 | RSS(KB) | RSS(MB) |"
    echo "|------|---------|---------|"
} > "$REPORT_FILE"

# 采样表：>30 行只保留每第 5 个采样点
if [ "$SAMPLES" -gt 30 ]; then
    awk 'NR>1 && (NR-1)%5==1 {printf "| %s | %s | %.1f |\n", $1, $2, $2/1024}' "$MEM_LOG" >> "$REPORT_FILE"
    echo "" >> "$REPORT_FILE"
    echo "_采样共 $SAMPLES 行，报告仅保留每第 5 个采样点（全量日志: $MEM_LOG）_" >> "$REPORT_FILE"
else
    awk 'NR>1 {printf "| %s | %s | %.1f |\n", $1, $2, $2/1024}' "$MEM_LOG" >> "$REPORT_FILE"
fi

# ─── 终端简版报告 ──────────────────────────────────────────
echo ""
echo "=== 性能测试报告 ==="
echo "标签:     $LABEL"
echo "文件数:   $FILE_COUNT"
echo "数据大小: $TOTAL_SIZE"
echo "索引大小: $INDEX_SIZE"
echo "DB 大小:  $DB_SIZE"
if [ "$SAMPLES" -gt 0 ]; then
    echo "内存峰值: ${PEAK_MB} MB"
    echo "内存均值: ${AVG_MB} MB"
    echo "采样次数: $SAMPLES"
else
    echo "内存数据: 无（监控未记录）"
fi
echo ""
echo "内存采样日志: $MEM_LOG"
echo "完整报表: $REPORT_FILE"

# ─── 清理 ──────────────────────────────────────────────────
rm -rf "$APP_DATA"
echo ""
echo "临时数据已清理: $APP_DATA"
