# -*- coding: utf-8 -*-
"""发布端：把 models-catalog.json 提交到模型仓库（并推送），完成"上线"这一步。

配合 `prepare_ppocrv6.py`（生成清单）使用：

    python scripts/models/prepare_ppocrv6.py --out D:\\dist\\catalog
    python scripts/models/publish_catalog.py --catalog D:\\dist\\catalog\\models-catalog.json ^
        --repo-dir D:\\repos\\link-searcher-models

脚本会：
  1. 校验清单（schema / 必填字段 / sha256 格式 / 版本号）；
  2. 可选 `--check-urls`：HEAD 每个 remote（hf-mirror 失败会自动试 huggingface.co）；
  3. 把清单复制到仓库根目录的 models-catalog.json；
  4. `git add/commit`（无变化则跳过），默认再 `git push`；
  5. 打印 commit id 与 raw 地址，便于立刻验证。

常用开关：
  --no-push     只提交不推送（本地预览）
  --dry-run     只打印将要做的事
  --branch      目标分支（默认 main）
  --check-urls  发布前探测资产可达性（推荐）
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import urllib.request

REQUIRED_DEP_FIELDS = ("version", "files")
HEX64 = re.compile(r"^[0-9a-f]{64}$")


def fail(msg):
    print("错误: %s" % msg, file=sys.stderr)
    return 2


def load_catalog(path):
    with open(path, encoding="utf-8") as f:
        cat = json.load(f)
    if cat.get("schema") != 1:
        raise SystemExit("清单 schema 必须是 1（当前 %r）" % cat.get("schema"))
    deps = cat.get("deps")
    if not isinstance(deps, dict) or not deps:
        raise SystemExit("清单里没有 deps")
    for dep, d in deps.items():
        for fld in REQUIRED_DEP_FIELDS:
            if fld not in d:
                raise SystemExit("deps.%s 缺少 %s" % (dep, fld))
        files = d["files"]
        if not files:
            raise SystemExit("deps.%s 没有 files" % dep)
        names = set()
        for f in files:
            for fld in ("remote", "local", "sha256"):
                if not f.get(fld):
                    raise SystemExit("deps.%s 的某个文件缺少 %s" % (dep, fld))
            if not HEX64.match(f["sha256"]):
                raise SystemExit("deps.%s: sha256 不是 64 位小写 hex: %r" % (dep, f["sha256"]))
            if f["local"] in names:
                raise SystemExit("deps.%s: local 重名 %r" % (dep, f["local"]))
            names.add(f["local"])
            if not f["remote"].startswith(("http://", "https://")):
                print("  提示: %s 不是绝对 URL，将由 tag/base_url 拼接" % f["remote"])
    return cat


def check_urls(cat, timeout=20):
    """HEAD 每个 remote；hf-mirror 失败时试 huggingface.co。"""
    ok = True
    for dep, d in cat["deps"].items():
        for f in d["files"]:
            url = f["remote"]
            candidates = [url]
            if "://hf-mirror.com/" in url:
                candidates.append(url.replace("://hf-mirror.com/", "://huggingface.co/"))
            status = None
            for u in candidates:
                try:
                    req = urllib.request.Request(u, method="HEAD")
                    with urllib.request.urlopen(req, timeout=timeout) as r:
                        status = r.status
                        if status == 200:
                            print("  ✓ %s (%d)" % (u, status))
                            break
                except Exception as e:  # noqa: BLE001
                    print("  ! %s -> %s" % (u, e))
            if status != 200:
                print("  ✗ 不可达: %s" % url)
                ok = False
    return ok


def git(repo, *args, capture=True):
    cmd = ["git", "-C", repo, *args]
    if capture:
        return subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8")
    return subprocess.run(cmd)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--catalog", required=True, help="prepare_ppocrv6.py 生成的 models-catalog.json")
    ap.add_argument("--repo-dir", required=True, help="模型仓库的本地克隆目录")
    ap.add_argument("--branch", default="main")
    ap.add_argument("--message", default=None)
    ap.add_argument("--check-urls", action="store_true", help="发布前探测资产可达性")
    ap.add_argument("--no-push", action="store_true")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    print("[1/4] 校验清单")
    cat = load_catalog(args.catalog)
    dep_names = ", ".join("%s=%s" % (k, v["version"]) for k, v in cat["deps"].items())
    print("  ok: %s" % dep_names)

    if args.check_urls:
        print("[2/4] 探测资产可达性")
        if not check_urls(cat):
            return fail("有资产不可达，已中止（可用 --no-check 场景下先跳过）")
    else:
        print("[2/4] (跳过 URL 探测；建议发布前加 --check-urls)")

    repo = os.path.abspath(args.repo_dir)
    if not os.path.isdir(os.path.join(repo, ".git")):
        return fail("%s 不是 git 仓库" % repo)

    dest = os.path.join(repo, "models-catalog.json")
    print("[3/4] 写入 %s" % dest)
    if args.dry_run:
        print("  (dry-run) 将复制 %s -> %s" % (args.catalog, dest))
    else:
        shutil.copyfile(args.catalog, dest)

    print("[4/4] 提交%s" % ("并推送" if not args.no_push else "") + "（分支 %s）" % args.branch)
    if args.dry_run:
        print("  (dry-run) git add models-catalog.json && git commit && %s"
              % ("" if args.no_push else "git push"))
        return 0

    r = git(repo, "symbolic-ref", "--short", "HEAD")
    current = (r.stdout or "").strip()
    if not current:
        current = (git(repo, "rev-parse", "--abbrev-ref", "HEAD").stdout or "").strip()
    if current and current != "HEAD" and current != args.branch:
        print("  当前分支 %s，切换到 %s" % (current, args.branch))
        r = git(repo, "checkout", args.branch)
        if r.returncode != 0:
            return fail("切换分支失败: %s" % (r.stderr or "").strip())

    git(repo, "add", "models-catalog.json")
    r = git(repo, "status", "--porcelain", "models-catalog.json")
    if not (r.stdout or "").strip():
        print("  清单无变化，无需提交")
    else:
        msg = args.message or ("catalog: " + dep_names)
        r = git(repo, "commit", "-m", msg)
        if r.returncode != 0:
            return fail("提交失败: %s" % (r.stderr or r.stdout or "").strip())
        head = (git(repo, "rev-parse", "--short", "HEAD").stdout or "").strip()
        print("  已提交 %s  (%s)" % (head, msg))

    if not args.no_push:
        r = git(repo, "push", "origin", args.branch)
        if r.returncode != 0:
            return fail("推送失败: %s" % (r.stderr or r.stdout or "").strip())
        print("  已推送 origin/%s" % args.branch)

    remote = (git(repo, "remote", "get-url", "origin").stdout or "").strip()
    print("\n完成。")
    if remote:
        m = re.search(r"github\.com[:/]([^/]+/[^/.]+)", remote)
        if m:
            raw = "https://raw.githubusercontent.com/%s/%s/models-catalog.json" % (m.group(1), args.branch)
            print("  应用侧清单地址（默认）：%s" % raw)
            print("  可用 `LINK_SEARCHER_MODELS_CATALOG=%s` 在本地/测试环境覆盖。" % raw)
    return 0


if __name__ == "__main__":
    sys.exit(main())
