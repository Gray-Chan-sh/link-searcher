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
BIN_NAME="link-searcher"

# 校验测试目录
if [ ! -d "$DATA_DIR" ]; then
    echo "错误: 测试目录不存在: $DATA_DIR"
    exit 1
fi

echo "=== Link-Searcher 性能测试 ==="
echo "数据目录: $DATA_DIR"
echo "标签:     $LABEL"
echo "应用数据: $APP_DATA"

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
echo "[1/3] 启动 $BIN_NAME（后台）..."

# 找到二进制：优先 release bundle，其次 cargo run
if command -v "$BIN_NAME" &>/dev/null; then
    BIN_PATH="$(command -v "$BIN_NAME")"
elif [ -f "src-tauri/target/release/link-searcher" ]; then
    BIN_PATH="src-tauri/target/release/link-searcher"
elif [ -f "src-tauri/target/debug/link-searcher" ]; then
    BIN_PATH="src-tauri/target/debug/link-searcher"
else
    echo "错误: 未找到 $BIN_NAME 二进制。请先构建: npm run tauri build"
    exit 1
fi
echo "二进制:   $BIN_PATH"

# 后台启动，指向临时数据目录
"$BIN_PATH" --data-dir "$APP_DATA" &
APP_PID=$!
echo "PID:      $APP_PID"

# 等待应用就绪（最多 15 秒）
echo "等待应用就绪..."
for i in $(seq 1 15); do
    if pgrep -p "$APP_PID" > /dev/null 2>&1; then
        # 进程存活即视为启动成功（CLI 模式无需等待窗口）
        break
    fi
    sleep 1
done

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

# ─── 生成报告 ──────────────────────────────────────────────
echo ""
echo "=== 性能测试报告 ==="
echo "标签:     $LABEL"
echo "文件数:   $FILE_COUNT"
echo "数据大小: $TOTAL_SIZE"
echo ""

# 索引/DB 大小
if [ -d "$APP_DATA/.ls-index" ]; then
    INDEX_SIZE=$(du -sh "$APP_DATA/.ls-index" 2>/dev/null | cut -f1)
else
    INDEX_SIZE="N/A（未找到索引目录）"
fi
if [ -f "$APP_DATA/data.db" ]; then
    DB_SIZE=$(du -sh "$APP_DATA/data.db" 2>/dev/null | cut -f1)
else
    DB_SIZE="N/A（未找到数据库）"
fi
echo "索引大小: $INDEX_SIZE"
echo "DB 大小:  $DB_SIZE"
echo ""

# 内存峰值
if [ -s "$MEM_LOG" ]; then
    PEAK_RSS=$(awk 'NR>1 {print $2}' "$MEM_LOG" | sort -nr | head -1)
    PEAK_MB=$(awk "BEGIN {printf \"%.1f\", $PEAK_RSS / 1024}")
    AVG_RSS=$(awk 'NR>1 {sum+=$2; n++} END {printf "%.0f", sum/n}' "$MEM_LOG")
    AVG_MB=$(awk "BEGIN {printf \"%.1f\", $AVG_RSS / 1024}")
    SAMPLES=$(awk 'NR>1' "$MEM_LOG" | wc -l | tr -d ' ')
    echo "内存峰值: ${PEAK_MB} MB ($PEAK_RSS KB)"
    echo "内存均值: ${AVG_MB} MB ($AVG_RSS KB)"
    echo "采样次数: $SAMPLES"
else
    echo "内存数据: 无（监控未记录）"
fi

echo ""
echo "内存采样日志: $MEM_LOG"

# ─── 清理 ──────────────────────────────────────────────────
rm -rf "$APP_DATA"
echo ""
echo "临时数据已清理: $APP_DATA"
