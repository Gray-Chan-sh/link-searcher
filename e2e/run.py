#!/usr/bin/env python3
"""可复跑的 AI 问答问题集执行器。

用法：
    set LS_WEB_TOKEN=<Web API token>
    python e2e/run.py --through 0                 # 只跑到 Step 0 的用例
    python e2e/run.py --through 3 --report e2e/reports/2026-11-08-step3.md

token 只从环境变量读，脚本本身不含任何密钥。
"""
import argparse
import json
import os
import re
import sqlite3
import ssl
import sys
import time
import urllib.request
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_DB = r"D:\index\data.db"
API = "https://127.0.0.1:8443/api/ai/conversation/ask/stream"


def load_cases(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def ask(msgs, sid, source_ids, token):
    body = json.dumps({"messages": msgs, "sessionId": sid,
                       "sourceIds": source_ids}).encode("utf-8")
    req = urllib.request.Request(API, data=body, headers={
        "Authorization": "Bearer " + token,
        "Content-Type": "application/json",
        "Accept": "text/event-stream",
    })
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    t0 = time.time()
    done = None
    with urllib.request.urlopen(req, context=ctx, timeout=300) as r:
        ev = None
        for raw in r:
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            if line.startswith("event:"):
                ev = line[6:].strip()
            elif line.startswith("data:") and ev == "ai-done":
                done = json.loads(line[5:].strip())
                break
    return done or {}, time.time() - t0


def ask_with_retry(msgs, sid, source_ids, token, attempts=4):
    """网关限流/断连时重试同一轮。

    `ai-done` 里出现「AI 网关拒绝」说明上游失败但应用把错误当正文返回了，
    这种情况也要重试，否则会把限流误判成回答质量问题。
    """
    last = ({}, 0.0)
    for i in range(attempts):
        time.sleep(5)
        try:
            done, dt = ask(msgs, sid, source_ids, token)
        except Exception as e:
            print(f"    ! 请求异常（第 {i + 1} 次）：{e}")
            time.sleep(20 * (i + 1))
            continue
        text = (done.get("full_text") or "")
        if "网关拒绝" in text or "429" in text:
            print(f"    ! 上游限流（第 {i + 1} 次），等待后重试")
            time.sleep(20 * (i + 1))
            last = (done, dt)
            continue
        return done, dt
    return last


def read_events(db, sid, turn):
    out = {}
    for attempt in range(8):
        try:
            con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
            rows = list(con.execute(
                "select event_type,payload_json from ai_events "
                "where session_id=? and turn_number=? order by event_seq", (sid, turn)))
            con.close()
        except Exception:
            rows = []
        for et, p in rows:
            d = json.loads(p)
            if et == "query_rewrite":
                out.update(rewritten=d.get("rewritten"), basis=d.get("basis"),
                           anchor=d.get("anchor") or [], needs=d.get("needs") or [],
                           refocused=d.get("refocused"))
            elif et == "gap_check":
                out.update(gap_checked=d.get("checked"),
                           gap_answerable=d.get("answerable"),
                           gap_error=d.get("error_kind"))
            elif et == "context_assembled":
                out.update(materials=d.get("material_count"), chars=d.get("total_chars"),
                           carried_count=d.get("carried_count"),
                           carried_chars=d.get("carried_chars"),
                           dropped=d.get("dropped"),
                           context_truncated=d.get("truncated"))
            elif et == "turn_complete":
                out.update(took_ms=d.get("took_ms"), truncated=d.get("truncated"))
        if "rewritten" in out:
            break
        time.sleep(0.8)
    return out


def evaluate(checks, fields):
    fails = []
    for c in checks:
        field, op, want = c["field"], c["op"], c["value"]
        got = fields.get(field)
        ok = False
        try:
            if got is None:
                # 事件没读到 ≠ 通过：以前 None 当 0，会让"没测到"伪装成 PASS。
                ok = op == "ne" and want is not None
            elif op == "eq":
                ok = got == want
            elif op == "ne":
                ok = got != want
            elif op == "contains":
                ok = isinstance(got, (list, str)) and (
                    (want in got) if isinstance(got, str) else any(want in x for x in got))
            elif op == "not_contains":
                ok = isinstance(got, (list, str)) and not (
                    (want in got) if isinstance(got, str) else any(want in x for x in got))
            elif op == "matches":
                ok = isinstance(got, str) and re.search(want, got) is not None
            elif op in ("<=", ">=", "<", ">"):
                got_v = got if got is not None else 0
                ok = {"<=": got_v <= want, ">=": got_v >= want,
                      "<": got_v < want, ">": got_v > want}[op]
            elif op == "in":
                ok = got in want
        except Exception:
            ok = False
        if not ok:
            fails.append(f"{field}{op}{want!r} 实际={got!r}")
    return fails


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--through", type=int, default=3, help="只跑 step<=N 的用例")
    ap.add_argument("--report", default="", help="markdown 报告输出路径")
    ap.add_argument("--db", default=os.environ.get("LS_DB", DEFAULT_DB))
    ap.add_argument("--only", default="", help="只跑指定 id（逗号分隔）")
    args = ap.parse_args()

    token = os.environ.get("LS_WEB_TOKEN", "").strip()
    if not token:
        print("缺少环境变量 LS_WEB_TOKEN（Web API token）。", file=sys.stderr)
        return 2

    data = load_cases(os.path.join(HERE, "questions.json"))
    cases = [c for c in data["cases"] if c.get("step", 0) <= args.through]
    if args.only:
        want = {x.strip() for x in args.only.split(",")}
        cases = [c for c in cases if c["id"] in want]

    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    results = []
    for case in cases:
        sid = f"e2e-{case['id'].lower()}-{stamp}"
        msgs, src = [], []
        rows, error = [], None
        print(f"\n=== {case['id']}  {case['title']}")
        for i, q in enumerate(case["turns"]):
            msgs.append({"role": "user", "content": q})
            try:
                done, dt = ask_with_retry(msgs, sid, src if case.get("carry") else [], token)
            except Exception as e:
                error = f"t{i} 请求失败: {e}"
                print("  " + error)
                break
            ans = done.get("full_text") or ""
            src = done.get("source_ids") or []
            ev = read_events(args.db, sid, i)
            ev.update(took_ms=ev.get("took_ms") or done.get("took_ms"),
                      answer=ans, answer_head=ans[:60])
            rows.append(ev)
            print(f"  t{i} {dt:5.0f}s basis={ev.get('basis')} q={str(ev.get('rewritten'))[:30]!r} "
                  f"anchor={ev.get('anchor')} mats={ev.get('materials')} "
                  f"chars={ev.get('chars')} carry={ev.get('carried_count')} "
                  f"gap={ev.get('gap_checked')}/{ev.get('gap_error')}")
            msgs.append({"role": "assistant", "content": ans})

        evaluated = rows if case.get("check_all_turns") else rows[-1:]
        fails, checked = [], []
        for ev in evaluated:
            checked.append(ev)
            fails += evaluate(case["checks"], ev)
        # 上游限流（重试后仍然 429）不是产品质量问题：单独记为 env，不计入失败。
        env = any("网关拒绝" in (r.get("answer") or "") for r in rows)
        ok = error is None and bool(rows) and not fails and not env
        if not rows:
            fails = fails or ["没有任何一轮完成"]
        results.append({"id": case["id"], "title": case["title"], "step": case.get("step", 0),
                        "ok": ok, "env": env, "fails": fails, "rows": rows, "error": error})
        tag = "SKIP(env)" if env else ("PASS" if ok else "FAIL")
        print(f"  => {tag}" + (f"  {fails[:2]}" if fails and not env else ""))

        # 单条用例结束后立刻落盘一份原始数据，避免长跑中断丢证据
        raw_dir = os.path.join(HERE, "reports", "raw")
        os.makedirs(raw_dir, exist_ok=True)
        with open(os.path.join(raw_dir, f"{stamp}-{case['id']}.json"), "w", encoding="utf-8") as f:
            json.dump({"id": case["id"], "ok": ok, "fails": fails, "rows": rows},
                      f, ensure_ascii=False, indent=2)

    passed = sum(1 for r in results if r["ok"])
    env_skip = sum(1 for r in results if r.get("env"))
    counted = len(results) - env_skip
    print(f"\n=== 汇总：{passed}/{counted} 通过（另有 {env_skip} 条因上游限流跳过）")
    for r in results:
        if not r["ok"] and not r.get("env"):
            print(f"  FAIL {r['id']}: {'; '.join(r['fails'][:3])}")

    if args.report:
        os.makedirs(os.path.dirname(os.path.abspath(args.report)), exist_ok=True)
        with open(args.report, "w", encoding="utf-8") as f:
            f.write(f"# e2e 报告 {stamp}（through step {args.through}）\n\n")
            f.write(f"通过 {passed}/{len(results)}\n\n")
            f.write("| 用例 | 步骤 | 结果 | 关键数据 |\n|---|---|---|---|\n")
            for r in results:
                last = r["rows"][-1] if r["rows"] else {}
                key = (f"basis={last.get('basis')} q={str(last.get('rewritten'))[:26]} "
                       f"mats={last.get('materials')} chars={last.get('chars')} "
                       f"carry={last.get('carried_count')} gap={last.get('gap_checked')}")
                f.write(f"| {r['id']} | {r['step']} | {'✅' if r['ok'] else '❌'} | {key} |\n")
            f.write("\n## 失败明细\n")
            if not any(not r["ok"] for r in results):
                f.write("无\n")
            for r in results:
                if not r["ok"]:
                    f.write(f"- **{r['id']}**: {'; '.join(r['fails'])}\n")
        print(f"报告已写入 {args.report}")

    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
