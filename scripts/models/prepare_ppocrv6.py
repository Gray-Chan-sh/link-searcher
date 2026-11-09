# -*- coding: utf-8 -*-
"""发布端：把 PP-OCRv6 tiny 打成"模型在线升级"可用的资产，并生成清单。

为什么不需要改 ONNX：
  - 应用走 ORT 后端时，原始 ONNX（符号 batch + value_info）**可以直接跑**；
  - 我们自己的 tract 后端才需要"batch 写死 / 去 value_info"的补丁，但 tract 0.20 对
    v6 rec 有已知数值 bug（见 `_probe/out/b1_findings.md`），所以 v6 只面向 ORT 用户。
  - 依赖中心升级前会跑自检推理：没有 ORT 的机器升级会被拦下并提示先装加速包，
    因此"v6 只在 ORT 下可用"是安全的，不会把用户的 OCR 搞坏。

用法：
  # 1) 直接从 HuggingFace 拉（需要网络）
  python scripts/models/prepare_ppocrv6.py --out D:\\dist\\models-v2

  # 2) 用本地已下载的模型（离线）
  python scripts/models/prepare_ppocrv6.py --out D:\\dist\\models-v2 --from-dir D:\\test\\_probe\\models\\v6tiny

产物：
  <out>/files/paddleocr-v6-det.onnx
  <out>/files/paddleocr-v6-rec.onnx
  <out>/files/paddleocr-v6-dict.txt
  <out>/models-catalog.json      ← 直接放到模型仓库 main 分支
  <out>/UPLOAD.md                ← 上传步骤（含 gh 命令与校验值）

依赖：onnx（校验）、pyyaml（解析 inference.yml）、numpy、onnxruntime（可选，用于自检）
"""

import argparse
import hashlib
import json
import os
import shutil
import sys

HF = "https://huggingface.co/PaddlePaddle"

DET_REPO = "PP-OCRv6_tiny_det_onnx"
REC_REPO = "PP-OCRv6_tiny_rec_onnx"

# 应用内 `deps::remote` 只认这些字段
SCHEMA = 1


def sha256_of(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download(url, dest):
    if os.path.isfile(dest):
        print("  cached:", dest)
        return
    try:
        import urllib.request

        os.makedirs(os.path.dirname(dest) or ".", exist_ok=True)
        print("  GET", url)
        with urllib.request.urlopen(url, timeout=120) as r, open(dest, "wb") as f:
            shutil.copyfileobj(r, f)
    except Exception as e:  # noqa: BLE001
        raise SystemExit("下载失败: %s\n%s" % (url, e))


def extract_dict(yml_path, out_path):
    """从 inference.yml 的 PostProcess.character_dict 抽字典（保持模型原始顺序）。"""
    try:
        import yaml

        with open(yml_path, encoding="utf-8") as f:
            doc = yaml.safe_load(f)
        chars = doc["PostProcess"]["character_dict"]
    except Exception as e:  # noqa: BLE001
        print("  ! PyYAML 解析失败（%s），改用逐行解析" % e)
        chars = []
        with open(yml_path, encoding="utf-8") as f:
            in_dict = False
            for line in f:
                if line.startswith("character_dict:"):
                    in_dict = True
                    continue
                if in_dict:
                    if not line.startswith("- ") and line.strip() and not line.startswith(" "):
                        break
                    s = line[2:].rstrip("\n")
                    if s.startswith("'") and s.endswith("'") and len(s) >= 2:
                        s = s[1:-1].replace("''", "'")
                    elif s.startswith('"') and s.endswith('"') and len(s) >= 2:
                        s = s[1:-1].replace('\\"', '"').replace("\\\\", "\\")
                    chars.append(s)
    if len(chars) < 1000:
        raise SystemExit("字典条目异常（%d），拒绝发布" % len(chars))
    with open(out_path, "w", encoding="utf-8", newline="\n") as f:
        for c in chars:
            f.write(c + "\n")
    print("  字典: %d 条" % len(chars))
    return len(chars)


def verify_with_ort(det, rec, dict_path):
    """可选自检：用样例图跑一遍，确认模型能出字（而不是静默全 blank）。"""
    try:
        import numpy as np
        import onnxruntime as ort
    except Exception as e:  # noqa: BLE001
        print("  (跳过自检: %s)" % e)
        return None
    import subprocess
    import tempfile

    # 用仓库里的自检图（应用侧同一张）
    img = os.path.join("src-tauri", "src", "extractor", "paddleocr", "smoke_text.png")
    if not os.path.isfile(img):
        print("  (跳过自检: 缺少 %s)" % img)
        return None

    from PIL import Image

    det_sess = ort.InferenceSession(det, providers=["CPUExecutionProvider"])
    rec_sess = ort.InferenceSession(rec, providers=["CPUExecutionProvider"])
    sample = Image.open(img).convert("RGB")
    # 检测：整图缩放到 max side 1280，并把宽高**补齐到 32 的倍数**（PP-OCR 标准做法；
    # 不补齐会在内部的 Add/Broadcast 处报 "15 by 16"）。
    w, h = sample.size
    scale = min(1.0, 1280.0 / max(w, h))
    nw, nh = max(1, int(round(w * scale))), max(1, int(round(h * scale)))
    nw32 = (nw + 31) // 32 * 32
    nh32 = (nh + 31) // 32 * 32
    resized = sample.resize((nw, nh), Image.BILINEAR)
    canvas = Image.new("RGB", (nw32, nh32), (255, 255, 255))
    canvas.paste(resized, (0, 0))
    arr = np.asarray(canvas, dtype=np.float32)[:, :, ::-1] / 255.0  # BGR
    mean = np.array([0.485, 0.456, 0.406], dtype=np.float32)
    std = np.array([0.229, 0.224, 0.225], dtype=np.float32)
    arr = (arr - mean) / std
    x = arr.transpose(2, 0, 1)[None, ...].astype(np.float32)
    prob = det_sess.run(None, {det_sess.get_inputs()[0].name: x})[0][0, 0]
    print("  自检：检测图 %.0f%% 像素 > 0.3" % (100.0 * float((prob > 0.3).mean())))
    if float((prob > 0.3).mean()) <= 0.0005:
        raise SystemExit("检测模型在自检图上没有响应（全背景）")
    # 识别：整图按 48 高送一次（只验证"能出字"，不追求版面）
    rec_in = sample.resize((min(320, max(32, int(48 * w / h))), 48), Image.BILINEAR)
    ra = np.asarray(rec_in, dtype=np.float32)[:, :, ::-1] / 255.0
    canvas = np.zeros((48, 320, 3), dtype=np.float32)
    canvas[:, : ra.shape[1], :] = ra
    rx = ((canvas - 0.5) / 0.5).transpose(2, 0, 1)[None, ...].astype(np.float32)
    logits = rec_sess.run(None, {rec_sess.get_inputs()[0].name: rx})[0][0]
    idx = logits.argmax(-1)
    chars = open(dict_path, encoding="utf-8").read().splitlines()
    text, prev = [], None
    for i in idx:
        i = int(i)
        if i == 0:
            continue
        if i == prev:
            continue
        if 1 <= i <= len(chars):
            text.append(chars[i - 1])
        prev = i
    s = "".join(text)
    print("  自检：识别出 %d 字 %r" % (len(s), s[:40]))
    if not s.strip():
        raise SystemExit("识别模型在自检图上没有输出任何文字")
    return s


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, help="输出目录")
    ap.add_argument("--from-dir", help="用本地模型目录（含 det.onnx/rec.onnx，可选 inference.yml）")
    ap.add_argument("--version", default="v6-tiny-1")
    ap.add_argument("--tag", default="models-v2")
    ap.add_argument("--min-app-version", default="1.5.16")
    ap.add_argument("--no-verify", action="store_true")
    args = ap.parse_args()

    out = os.path.abspath(args.out)
    files_dir = os.path.join(out, "files")
    os.makedirs(files_dir, exist_ok=True)
    src = os.path.abspath(args.from_dir) if args.from_dir else None

    print("[1/4] 取模型文件")
    if src:
        pairs = [
            (os.path.join(src, "det.onnx"), "paddleocr-v6-det.onnx"),
            (os.path.join(src, "rec.onnx"), "paddleocr-v6-rec.onnx"),
        ]
        for s, name in pairs:
            if not os.path.isfile(s):
                raise SystemExit("缺少 %s" % s)
            shutil.copyfile(s, os.path.join(files_dir, name))
            print("  copy", os.path.basename(s), "->", name)
        yml = os.path.join(src, "inference.yml")
        if not os.path.isfile(yml):
            yml = os.path.join(src, "rec_inference.yml")
    else:
        yml = os.path.join(out, "download", "inference.yml")
        download("%s/%s/resolve/main/inference.onnx" % (HF, DET_REPO), os.path.join(files_dir, "paddleocr-v6-det.onnx"))
        download("%s/%s/resolve/main/inference.onnx" % (HF, REC_REPO), os.path.join(files_dir, "paddleocr-v6-rec.onnx"))
        download("%s/%s/resolve/main/inference.yml" % (HF, REC_REPO), yml)

    print("[2/4] 抽识别字典")
    dict_out = os.path.join(files_dir, "paddleocr-v6-dict.txt")
    if os.path.isfile(yml):
        extract_dict(yml, dict_out)
    else:
        raise SystemExit("找不到 inference.yml，无法抽字典")

    print("[3/4] 可选自检")
    if args.no_verify:
        print("  (已跳过)")
    else:
        verify_with_ort(
            os.path.join(files_dir, "paddleocr-v6-det.onnx"),
            os.path.join(files_dir, "paddleocr-v6-rec.onnx"),
            dict_out,
        )

    print("[4/4] 生成清单")
    files = []
    local_names = {
        "paddleocr-v6-det.onnx": "det.onnx",
        "paddleocr-v6-rec.onnx": "rec.onnx",
        "paddleocr-v6-dict.txt": "dict.txt",
    }
    for name in ("paddleocr-v6-det.onnx", "paddleocr-v6-rec.onnx", "paddleocr-v6-dict.txt"):
        p = os.path.join(files_dir, name)
        size = os.path.getsize(p)
        entry = {
            "remote": name,
            "local": local_names[name],
            "sha256": sha256_of(p),
            "size": size,
        }
        files.append(entry)
        print("  %-24s %8.2f MB  %s…" % (name, size / 1048576, entry["sha256"][:16]))

    total = sum(f["size"] for f in files)
    catalog = {
        "schema": SCHEMA,
        "generated_at": None,
        "deps": {
            "paddleocr": {
                "version": args.version,
                "tag": args.tag,
                "min_app_version": args.min_app_version,
                "size_bytes": total,
                "notes": "PP-OCRv6 tiny：体积约 1/3、速度约 4 倍（需 ONNX Runtime 加速包）",
                "files": files,
            }
        },
    }
    cat_path = os.path.join(out, "models-catalog.json")
    with open(cat_path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(catalog, f, ensure_ascii=False, indent=2)
        f.write("\n")

    with open(os.path.join(out, "UPLOAD.md"), "w", encoding="utf-8", newline="\n") as f:
        f.write("# 上传步骤\n\n")
        f.write("1. 新建/复用 Release tag `%s`，上传 `files/` 下三个文件（文件名保持原样）：\n\n" % args.tag)
        for it in files:
            f.write("- `%s`  sha256 `%s`  %d bytes\n" % (it["remote"], it["sha256"], it["size"]))
        f.write("\n2. 把 `models-catalog.json` 提交到模型仓库 `main` 分支根目录。\n\n")
        f.write("3. 应用侧：依赖中心 →「检查模型更新」→「升级」。\n\n")
        f.write("> 注意：v6 需要 ONNX Runtime 加速包；没装的机器升级时会被自检拦下（旧版本保持不变）。\n")

    print("\n完成：")
    print("  资产:", files_dir)
    print("  清单:", cat_path)
    print("  说明:", os.path.join(out, "UPLOAD.md"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
