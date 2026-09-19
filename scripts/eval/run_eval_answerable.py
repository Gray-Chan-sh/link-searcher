#!/usr/bin/env python3
"""
可答率评测：对每道题的 top-10 文档，用 LLM judge 判断"能否回答"。

与 run_rag_eval.sh 的区别：
  run_rag_eval.sh  只检查 support_files 是否出现在 top-10（指定文件匹配）
  本脚本           检查 top-10 里是否有任意一份文档能回答问题（可答率）

用法：
  python3 scripts/eval/run_eval_answerable.py <golden.jsonl> <db_path> [--llm-url URL] [--llm-key KEY] [--llm-model MODEL]

环境变量（可选）：
  LINK_SEARCHER_DATA_DIR    数据库路径（与 db_path 参数等效）
  EVAL_LLM_URL             LLM API 地址
  EVAL_LLM_KEY             LLM API 密钥
  EVAL_LLM_MODEL           LLM 模型名
  LS_BIN                   link-searcher 二进制路径（默认 src-tauri/target/debug/link-searcher）
  LINK_SEARCHER_VOCAB_REWRITE  on/off（控制 Level 1 改写开关）
"""
import json, os, re, subprocess, sys, sqlite3, time, urllib.request
from pathlib import Path
from collections import defaultdict

def load_llm_config():
    """读程序 config，解析 active_llm_model_id → (base_url, api_key, model)"""
    # 优先 env var
    url = os.environ.get("EVAL_LLM_URL")
    key = os.environ.get("EVAL_LLM_KEY")
    model = os.environ.get("EVAL_LLM_MODEL")
    if url and key and model:
        return url, key, model

    # 读 config.json
    config_paths = [
        Path.home() / "Library" / "Application Support" / ".link-searcher" / "config.json",
        Path.home() / "Library" / "Application Support" / "link-searcher" / "config.json",
    ]
    for cp in config_paths:
        if cp.exists():
            try:
                c = json.loads(cp.read_text())
                active = c.get("active_llm_model_id", "")
                if ":" not in active:
                    continue
                pid, mid = active.split(":", 1)
                for p in c.get("providers", []):
                    if p.get("id") == pid and p.get("base_url"):
                        return p["base_url"], p.get("api_key", ""), mid
            except Exception:
                continue

    print(
        "[error] 未能从程序 config 解析出 LLM 端点。\n"
        "        请显式传入 --llm-url/--llm-key/--llm-model，"
        "或先在 Link-Searcher 设置页配置 AI Provider（active_llm_model_id）。",
        file=sys.stderr,
    )
    sys.exit(2)


def llm_judge(base_url, api_key, model, question, doc_text, retries=3):
    """用 LLM 判断文档能否回答问题，返回 (bool, raw_response)"""
    system = (
        "你是文档检索质量评估助手。用户提出一个问题，下面是一份文档内容的前3000字。"
        "请判断：仅凭这份文档提供的信息，能否回答该问题？"
        "只要文档里包含回答该问题所需的实质性信息（事实、数字、条款、程序等）就算'能回答'；"
        "即使答案需从多句话或条款综合得出也算'能回答'。"
        "只看文档内容，不用外部知识或常识补充。"
        "只输出一个词：能回答 或 不能回答"
    )
    user = f"问题：{question}\n\n文档内容（前3000字）：\n{doc_text[:3000]}"
    data = json.dumps({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": 20,
        "temperature": 0.0,
    }).encode()

    url = f"{base_url.rstrip('/')}/chat/completions"
    for attempt in range(retries):
        try:
            req = urllib.request.Request(url, data=data, headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {api_key}",
            })
            raw = urllib.request.urlopen(req, timeout=60).read().decode()
            # 清理 SSE trailing
            raw = re.sub(r'\n?data: \[DONE\]\s*$', '', raw.strip())
            r = json.loads(raw)
            resp = r["choices"][0]["message"]["content"].strip()
            return resp.startswith("能"), resp
        except Exception as e:
            if attempt < retries - 1:
                time.sleep(2 * (attempt + 1))
            else:
                return False, f"LLM error: {e}"
    return False, "max retries"


def run_dry_run(bin_path, question, data_dir):
    """运行 link-searcher chat --dry-run，解析 top-10 文档路径"""
    env = os.environ.copy()
    env["LINK_SEARCHER_DATA_DIR"] = data_dir
    r = subprocess.run(
        [bin_path, "chat", "--dry-run", question],
        capture_output=True, text=True, timeout=120,
        env=env,
    )
    out = r.stdout + r.stderr
    docs = []
    for line in out.splitlines():
        m = re.match(r'\[\s*(\d+)\]\s+bm25=(\S+)\s+sem=(\S+)\s+path=(.+)', line.strip())
        if m:
            rank = int(m.group(1))
            path = m.group(4).strip()
            bm25 = float(m.group(2)) if m.group(2) != "-" else None
            sem = float(m.group(3)) if m.group(3) != "-" else None
            docs.append({"rank": rank, "path": path, "bm25": bm25, "sem": sem})
    return docs[:10]


def main():
    import argparse
    parser = argparse.ArgumentParser(description="可答率评测（LLM judge）")
    parser.add_argument("golden", help="golden.jsonl 文件路径")
    parser.add_argument("data_dir", help="link-searcher 数据目录")
    parser.add_argument("--llm-url", help="LLM API URL（覆盖 config）")
    parser.add_argument("--llm-key", help="LLM API key")
    parser.add_argument("--llm-model", help="LLM model name")
    parser.add_argument("--bin", default=os.environ.get("LS_BIN", "src-tauri/target/debug/link-searcher"))
    args = parser.parse_args()

    golden_path = Path(args.golden)
    rows = [json.loads(l) for l in golden_path.read_text().splitlines() if l.strip()]

    if args.llm_url and args.llm_key and args.llm_model:
        llm_url, llm_key, llm_model = args.llm_url, args.llm_key, args.llm_model
    else:
        llm_url, llm_key, llm_model = load_llm_config()
    print(f"[LLM] {llm_model} @ {llm_url}")
    print(f"[DB]  {args.data_dir}")
    print(f"[BIN] {args.bin}")

    conn = sqlite3.connect(os.path.join(args.data_dir, "data.db"))
    total = len(rows)
    hit = 0
    cat_stats = defaultdict(lambda: {"total": 0, "hit": 0, "judge_yes": 0, "judge_no": 0, "judge_err": 0})

    for i, row in enumerate(rows):
        q = row["question"]
        supports = row.get("support_files", [])
        cat = row.get("category", "uncategorized")

        st = cat_stats[cat]
        st["total"] += 1

        # 1. dry-run 取 top-10
        try:
            top10 = run_dry_run(args.bin, q, args.data_dir)
        except Exception as e:
            print(f"  [{i+1:>3}/{total}] dry-run error: {e}")
            st["judge_err"] += 1
            continue

        # 2. 对 top-10 每份文档，读文本 → judge
        q_hit = False
        for doc in top10:
            name = Path(doc["path"]).name
            rows_doc = conn.execute(
                "SELECT ci.text_content FROM file_tracking ft "
                "JOIN content_index ci ON ft.md5=ci.md5 "
                "WHERE ft.path LIKE ? LIMIT 1",
                (f"%{name}%",)
            ).fetchall()

            if not rows_doc:
                st["judge_err"] += 1
                continue

            text = rows_doc[0][0]
            can_answer, resp = llm_judge(llm_url, llm_key, llm_model, q, text)

            if can_answer:
                q_hit = True
                st["judge_yes"] += 1
                break  # 一份能答就够了
            else:
                st["judge_no"] += 1

        if q_hit:
            hit += 1
            st["hit"] += 1

        # 打印进度（每5题）
        if (i + 1) % 5 == 0 or i == 0:
            print(f"  [{i+1:>3}/{total}] answerable: {hit}/{i+1} ({hit/(i+1):.1%})")

    conn.close()

    # 汇总
    answerable_rate = hit / total if total else 0.0
    print(f"\n{'='*60}")
    print(f"  可答率评测结果（{total} 问句，LLM judge: {llm_model}）")
    print(f"{'='*60}")
    print(f"  Answerable@10: {answerable_rate:.2%} ({hit}/{total})")
    print()

    if cat_stats:
        print("  按类别（category）:")
        for cat in sorted(cat_stats):
            s = cat_stats[cat]
            rate = s["hit"] / s["total"] if s["total"] else 0.0
            print(f"    {cat:<14} {s['total']:>3} 题  Answerable {rate:6.2%} ({s['hit']}/{s['total']})  "
                  f"judge:yes={s['judge_yes']} no={s['judge_no']} err={s['judge_err']}")


if __name__ == "__main__":
    main()
