#!/usr/bin/env bash
# ===========================================================================
# RAG 检索层评测（Context Recall@10）
# 用法: bash scripts/eval/run_rag_eval.sh <golden_dir> [data_dir]
#   golden_dir  含评测语料目录 docs/ + golden.jsonl（问句→支撑文件标注）
#   data_dir    评测用独立 data dir（默认 mktemp，隔离真实库）
#
# 原理：
#   golden 的每个问题跑 link-searcher chat --dry-run（3 路检索 + 注入，
#   不调用 LLM），解析 "[N] ... path=..." 输出取 top-10 文件，
#   与该问句标注的支撑文件比对 → Context Recall@10 / Success@10。
#
# 注意：dry-run 依赖 embedding 配置；未配置本地 BGE 时向量通道静默跳过，
#   结果只反映 BM25+路径通道 —— 评测前需确认 active_embedding_model_id。
# ===========================================================================
set -euo pipefail

GOLDEN_DIR="${1:?用法: run_rag_eval.sh <golden_dir> [data_dir]}"
DATA_DIR="${2:-}"
BIN="${LS_BIN:-src-tauri/target/debug/link-searcher}"
cd "$(cd "$(dirname "$0")/../.." && pwd)"

if [ ! -f "${BIN}" ]; then
  echo "❌ 未找到 ${BIN}，先构建: (cd src-tauri && cargo build)" >&2
  exit 1
fi
if [ ! -f "${GOLDEN_DIR}/golden.jsonl" ]; then
  echo "❌ ${GOLDEN_DIR}/golden.jsonl 不存在（问句→支撑文件标注）" >&2
  exit 1
fi

# 用独立 data dir 隔离评测，不污染真实库
if [ -z "${DATA_DIR}" ]; then
  DATA_DIR="$(mktemp -d /tmp/ls-eval-XXXX)"
  echo "📁 临时 data dir: ${DATA_DIR}"
  export LINK_SEARCHER_DATA_DIR="${DATA_DIR}"
  "${BIN}" scan "${GOLDEN_DIR}/docs" >/dev/null 2>&1 || {
    echo "❌ 索引 ${GOLDEN_DIR}/docs 失败" >&2
    exit 1
  }
  echo "✅ 已索引 $(find "${GOLDEN_DIR}/docs" -type f | wc -l | tr -d ' ') 个文档"
else
  export LINK_SEARCHER_DATA_DIR="${DATA_DIR}"
fi

# python 主逻辑：读 golden + 逐问跑 dry-run + 统计
GOLDEN_DIR="${GOLDEN_DIR}" BIN="${BIN}" python3 <<'PY'
import json, os, subprocess, sys
from pathlib import Path

golden_dir = Path(os.environ["GOLDEN_DIR"])
bin_path = os.environ["BIN"]
rows = [json.loads(l) for l in (golden_dir / "golden.jsonl").read_text().splitlines() if l.strip()]

total = hit_any = hit_files_total = support_total = 0
misses = []
for row in rows:
    q = row["question"]
    supports = row.get("support_files", [])
    total += 1
    support_total += len(supports)
    # dry-run 输出
    r = subprocess.run([bin_path, "chat", "--dry-run", q], capture_output=True, text=True, timeout=300)
    out = r.stdout + r.stderr
    # 解析 path= 行，取前 10 个 basename
    top10 = []
    for m in __import__("re").finditer(r"path=(\S+)", out):
        top10.append(Path(m.group(1)).name)
        if len(top10) >= 10:
            break
    q_hit = sum(1 for f in supports if f in top10)
    hit_files_total += q_hit
    if q_hit > 0:
        hit_any += 1
    else:
        misses.append(q)

print()
print("============================================")
print(f"  RAG 检索评测结果（{total} 问句）")
print("============================================")
recall = hit_files_total / support_total if support_total else 0.0
success = hit_any / total if total else 0.0
print(f"  Context Recall@10: {recall:.2%} ({hit_files_total}/{support_total})")
print(f"  Success@10 (≥1 支撑文件进 top-10): {success:.2%} ({hit_any}/{total})")
if misses:
    print()
    print("未命中问句（需人工检查检索质量）:")
    for m in misses:
        print(f"  - {m}")
PY
