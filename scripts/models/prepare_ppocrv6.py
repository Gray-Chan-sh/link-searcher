# -*- coding: utf-8 -*-
"""发布端：生成 PP-OCRv6 tiny 的在线升级清单（**直连上游 HuggingFace**，零自托管）。

设计（A 方案）：
  - 模型文件直接从 HuggingFace（经 hf-mirror.com）下载，我们**不重新托管**；
  - 识别字典上游没有独立文件，它在 `inference.yml` 的 `character_dict` 里 ——
    清单里标 `"dict_from_yaml": true`，应用下载 yml 后自己抽成 `dict.txt`；
  - 路径必须 pin 到**commit revision**（不能用 main），否则上游一改文件，
    我们钉的 sha256 就对不上，用户升级会失败；
  - 发布 = 提交一份 catalog JSON，**不需要 gh release / 上传任何资产**。

用法：
  python scripts/models/prepare_ppocrv6.py --out D:\\dist\\catalog
  python scripts/models/prepare_ppocrv6.py --out D:\\dist\\catalog --no-verify
  python scripts/models/prepare_ppocrv6.py --out D:\\dist\\catalog --revision-det <sha> --revision-rec <sha>

产物：<out>/models-catalog.json（提交到模型仓库 main 分支根目录）
"""

import argparse
import hashlib
import json
import os
import sys
import urllib.request

DET_REPO = "PP-OCRv6_tiny_det_onnx"
REC_REPO = "PP-OCRv6_tiny_rec_onnx"
HF_API = "https://huggingface.co/api/models/PaddlePaddle/%s"
MIRROR = "https://hf-mirror.com/PaddlePaddle/%s/resolve/%s/%s"
CANONICAL = "https://huggingface.co/PaddlePaddle/%s/resolve/%s/%s"


def resolve_revision(repo, override=None):
    if override:
        return override
    with urllib.request.urlopen(HF_API % repo, timeout=30) as r:
        return json.load(r)["sha"]


def sha256_of(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def fetch(repo, rev, name, dest):
    """镜像优先，官方兜底；已存在则复用。"""
    if os.path.isfile(dest):
        return dest
    os.makedirs(os.path.dirname(dest) or ".", exist_ok=True)
    for tpl in (MIRROR, CANONICAL):
        url = tpl % (repo, rev, name)
        try:
            print("  GET", url)
            with urllib.request.urlopen(url, timeout=180) as r, open(dest, "wb") as f:
                while True:
                    chunk = r.read(1 << 20)
                    if not chunk:
                        break
                    f.write(chunk)
            return dest
        except Exception as e:  # noqa: BLE001
            print("   ! %s" % e)
            if os.path.exists(dest):
                os.remove(dest)
    raise SystemExit("下载失败：%s/%s@%s" % (repo, name, rev))


def parse_dict(yml_path):
    """从 inference.yml 抽 character_dict（应用侧 Rust 也是同一套规则）。"""
    chars, in_dict = [], False
    with open(yml_path, encoding="utf-8") as f:
        for line in f:
            if not in_dict:
                if line.lstrip().startswith("character_dict:"):
                    in_dict = True
                continue
            s = line.lstrip()
            if s.startswith("- "):
                v = s[2:].rstrip("\n")
                if len(v) >= 2 and v[0] == "'" and v[-1] == "'":
                    v = v[1:-1].replace("''", "'")
                elif len(v) >= 2 and v[0] == '"' and v[-1] == '"':
                    v = v[1:-1].replace('\\"', '"').replace("\\\\", "\\")
                chars.append(v)
            elif s.strip():
                break
    if len(chars) < 1000:
        raise SystemExit("字典条目异常（%d）" % len(chars))
    return chars


def verify(det, rec, chars):
    """用仓库里的 smoke_text.png 跑一遍（det 要检出区域，rec 要出字）。"""
    try:
        import numpy as np
        import onnxruntime as ort
        from PIL import Image
    except Exception as e:  # noqa: BLE001
        print("  (跳过自检: %s)" % e)
        return
    img = os.path.join("src-tauri", "src", "extractor", "paddleocr", "smoke_text.png")
    if not os.path.isfile(img):
        print("  (跳过自检: 缺少 %s)" % img)
        return

    det_sess = ort.InferenceSession(det, providers=["CPUExecutionProvider"])
    rec_sess = ort.InferenceSession(rec, providers=["CPUExecutionProvider"])
    sample = Image.open(img).convert("RGB")
    w, h = sample.size
    scale = min(1.0, 1280.0 / max(w, h))
    nw, nh = max(1, int(round(w * scale))), max(1, int(round(h * scale)))
    canvas = Image.new("RGB", ((nw + 31) // 32 * 32, (nh + 31) // 32 * 32), (255, 255, 255))
    canvas.paste(sample.resize((nw, nh), Image.BILINEAR), (0, 0))
    arr = np.asarray(canvas, dtype=np.float32)[:, :, ::-1] / 255.0
    arr = (arr - np.array([0.485, 0.456, 0.406], np.float32)) / np.array([0.229, 0.224, 0.225], np.float32)
    prob = det_sess.run(None, {det_sess.get_inputs()[0].name: arr.transpose(2, 0, 1)[None].astype(np.float32)})[0][0, 0]
    ratio = float((prob > 0.3).mean())
    print("  det: %.2f%% 像素 > 0.3" % (100 * ratio))
    if ratio <= 0.0005:
        raise SystemExit("检测模型无响应")

    rw = min(320, max(32, int(48 * w / h)))
    ra = np.asarray(sample.resize((rw, 48), Image.BILINEAR), dtype=np.float32)[:, :, ::-1] / 255.0
    pad = np.zeros((48, 320, 3), np.float32)
    pad[:, :rw, :] = ra
    logits = rec_sess.run(None, {rec_sess.get_inputs()[0].name: ((pad - 0.5) / 0.5).transpose(2, 0, 1)[None].astype(np.float32)})[0][0]
    text, prev = [], None
    for i in logits.argmax(-1):
        i = int(i)
        if i == 0:
            continue
        if i == prev:
            continue
        if 1 <= i <= len(chars):
            text.append(chars[i - 1])
        prev = i
    s = "".join(text)
    print("  rec: %d 字 %r" % (len(s), s[:40]))
    if not s.strip():
        raise SystemExit("识别模型没有输出任何文字")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--cache", default=None, help="下载缓存目录（默认 <out>/cache）")
    ap.add_argument("--version", default="v6-tiny-1")
    ap.add_argument("--min-app-version", default="1.5.17")
    ap.add_argument("--revision-det", default=None)
    ap.add_argument("--revision-rec", default=None)
    ap.add_argument("--no-verify", action="store_true")
    args = ap.parse_args()

    out = os.path.abspath(args.out)
    cache = os.path.abspath(args.cache) if args.cache else os.path.join(out, "cache")
    os.makedirs(cache, exist_ok=True)

    print("[1/3] 解析上游 revision（pin 到 commit，避免上游改文件）")
    rev_det = resolve_revision(DET_REPO, args.revision_det)
    rev_rec = resolve_revision(REC_REPO, args.revision_rec)
    print("  det:", rev_det)
    print("  rec:", rev_rec)

    print("[2/3] 取文件并计算摘要")
    det = fetch(DET_REPO, rev_det, "inference.onnx", os.path.join(cache, "det.onnx"))
    rec = fetch(REC_REPO, rev_rec, "inference.onnx", os.path.join(cache, "rec.onnx"))
    yml = fetch(REC_REPO, rev_rec, "inference.yml", os.path.join(cache, "inference.yml"))
    chars = parse_dict(yml)
    print("  字典: %d 条（运行时由应用从 yml 抽取）" % len(chars))

    if args.no_verify:
        print("[3/3] (已跳过自检)")
    else:
        print("[3/3] 自检")
        verify(det, rec, chars)

    def entry(path, repo, rev, name, local, **extra):
        e = {
            "remote": MIRROR % (repo, rev, name),
            "local": local,
            "sha256": sha256_of(path),
            "size": os.path.getsize(path),
        }
        e.update(extra)
        return e

    files = [
        entry(det, DET_REPO, rev_det, "inference.onnx", "det.onnx"),
        entry(rec, REC_REPO, rev_rec, "inference.onnx", "rec.onnx"),
        entry(yml, REC_REPO, rev_rec, "inference.yml", "dict.txt", dict_from_yaml=True),
    ]
    catalog = {
        "schema": 1,
        "deps": {
            "paddleocr": {
                "version": args.version,
                "tag": "upstream-hf",
                "min_app_version": args.min_app_version,
                "size_bytes": sum(f["size"] for f in files),
                "notes": "PP-OCRv6 tiny：直连 HuggingFace；需 ONNX Runtime 加速包",
                "files": files,
            }
        },
    }
    path = os.path.join(out, "models-catalog.json")
    os.makedirs(out, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(catalog, f, ensure_ascii=False, indent=2)
        f.write("\n")
    print("\n清单:", path)
    for it in files:
        print("  %-10s %8.2f MB  %s" % (it["local"], it["size"] / 1048576, it["sha256"][:16] + "…"))
    print("\n下一步：把 models-catalog.json 提交到模型仓库 main 分支根目录即可（无需上传资产）。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
