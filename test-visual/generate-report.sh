#!/bin/bash
# ==============================================================================
# 生成 HTML 视觉测试报告
# 输入: $RESULTS_FILE (results.tsv) + $TESTOUT/ (截图目录)
# 输出: $TEST_VISUAL_DIR/report.html
# ==============================================================================

set -euo pipefail

TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")" && pwd)}"
RESULTS_FILE="${RESULTS_FILE:-$TEST_VISUAL_DIR/results.tsv}"
TESTOUT="${TESTOUT:-$TEST_VISUAL_DIR/current}"
REPORT="$TEST_VISUAL_DIR/report.html"

# 统计
total=0; passed=0; failed=0; skipped=0
while IFS='|' read -r id name status ts dir; do
    total=$((total + 1))
    case "$status" in
        PASS) passed=$((passed + 1)) ;;
        FAIL) failed=$((failed + 1)) ;;
        SKIP) skipped=$((skipped + 1)) ;;
    esac
done < "$RESULTS_FILE"

# 页眉
cat > "$REPORT" <<EOF
<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Link-Searcher GUI 测试报告</title>
  <style>
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; background: #111; color: #ddd; padding: 24px; }
    h1 { font-size: 24px; margin-bottom: 8px; }
    .meta { color: #888; font-size: 13px; margin-bottom: 24px; }
    .summary { display: flex; gap: 16px; margin-bottom: 32px; }
    .summary .card { padding: 12px 20px; border-radius: 8px; background: #1e1e1e; text-align: center; min-width: 80px; }
    .summary .card .num { font-size: 28px; font-weight: 700; }
    .summary .card .label { font-size: 12px; color: #888; margin-top: 4px; }
    .card.pass { border-left: 3px solid #4ade80; } .card.pass .num { color: #4ade80; }
    .card.fail { border-left: 3px solid #f87171; } .card.fail .num { color: #f87171; }
    .card.skip { border-left: 3px solid #fbbf24; } .card.skip .num { color: #fbbf24; }
    .case { margin: 20px 0; padding: 16px; border-radius: 8px; background: #1a1a1a; }
    .case.pass { border-left: 4px solid #4ade80; }
    .case.fail { border-left: 4px solid #f87171; }
    .case.skip { border-left: 4px solid #fbbf24; opacity: 0.6; }
    .case h2 { font-size: 16px; margin-bottom: 8px; }
    .case h2 .icon { font-size: 20px; margin-right: 6px; }
    .case .steps { display: flex; gap: 12px; flex-wrap: wrap; margin-top: 12px; }
    .case .step { text-align: center; font-size: 12px; }
    .case .step img { width: 200px; max-height: 150px; object-fit: cover; border: 2px solid #333; border-radius: 4px; }
    .case .step .label { color: #888; margin-top: 4px; }
    .case .step .diff img { border-color: #f87171; }
    .case .assertion { font-size: 12px; color: #888; margin-top: 8px; font-family: 'SF Mono', monospace; }
    .case .assertion .pass { color: #4ade80; }
    .case .assertion .fail { color: #f87171; }
    .progress-bar { height: 8px; border-radius: 4px; background: #333; margin-top: 8px; overflow: hidden; }
    .progress-bar .fill { height: 100%; border-radius: 4px; transition: width 0.3s; }
  </style>
</head>
<body>
<h1>Link-Searcher GUI 测试报告</h1>
<div class="meta">生成于 $(date '+%Y-%m-%d %H:%M:%S')</div>

<div class="summary">
  <div class="card pass"><div class="num">$passed</div><div class="label">✅ 通过</div></div>
  <div class="card fail"><div class="num">$failed</div><div class="label">❌ 失败</div></div>
  <div class="card skip"><div class="num">$skipped</div><div class="label">⏭ 跳过</div></div>
  <div class="card"><div class="num">$total</div><div class="label">总计</div></div>
</div>
<div class="progress-bar"><div class="fill" style="width:$(awk "BEGIN {printf \"%.0f\", ($passed/$total)*100}")%; background:#4ade80;"></div></div>

EOF

# 用例区块
while IFS='|' read -r id name status ts dir; do
    icon="✅"
    case "$status" in
        FAIL) icon="❌" ;;
        SKIP) icon="⏭" ;;
    esac
    status_class="pass"
    [ "$status" = "FAIL" ] && status_class="fail"
    [ "$status" = "SKIP" ] && status_class="skip"

    cat >> "$REPORT" <<EOF
<div class="case $status_class">
  <h2><span class="icon">$icon</span> [$id] $name</h2>
  <div class="steps">
EOF

    # 截图
    id_lower="$(printf '%s' "$id" | tr '[:upper:]' '[:lower:]')"
    if [ -d "$dir" ]; then
        for img in "$dir"/step-*.png; do
            [ -f "$img" ] || continue
            base=$(basename "$img")
            relpath="current/${id_lower}/${base}"
            steplabel=$(echo "$base" | sed 's/step-//;s/\.png//')
            cat >> "$REPORT" <<EOF
    <div class="step">
      <img src="$relpath" loading="lazy" alt="step $steplabel">
      <div class="label">Step $steplabel</div>
    </div>
EOF
        done
    fi

    # 差异图 (如有)
    diff_dir="$TEST_VISUAL_DIR/diff/${id_lower}"
    if [ -d "$diff_dir" ]; then
        for dimg in "$diff_dir"/step-*.png; do
            [ -f "$dimg" ] || continue
            base=$(basename "$dimg")
            relpath="diff/${id_lower}/${base}"
            cat >> "$REPORT" <<EOF
    <div class="step"><div class="diff">
      <img src="$relpath" loading="lazy" alt="diff">
      <div class="label" style="color:#f87171;">⚠ diff</div>
    </div></div>
EOF
        done
    fi

    cat >> "$REPORT" <<EOF
  </div>
</div>
EOF
done < "$RESULTS_FILE"

# 页脚
cat >> "$REPORT" <<EOF
</body>
</html>
EOF

echo "  ✅ 报告已生成: $REPORT"