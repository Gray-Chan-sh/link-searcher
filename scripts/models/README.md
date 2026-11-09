# 发布模型资产（在线升级用）

`prepare_ppocrv6.py`：把 PP-OCRv6 tiny 整理成"模型在线升级"能吃的资产，并生成
`models-catalog.json`（应用从模型仓库 `main` 分支的 raw 文件拉取）。

## 用法

```bash
# 从 HuggingFace 直接拉（需网络）
python scripts/models/prepare_ppocrv6.py --out D:\dist\models-v2

# 或用本地已下载的模型（离线）
python scripts/models/prepare_ppocrv6.py ^
  --out D:\dist\models-v2 ^
  --from-dir D:\test\_probe\models\v6src ^
  --version v6-tiny-1 --tag models-v2
```

产物：

```
D:\dist\models-v2\
├─ files\                     ← 上传到 GitHub Release（tag 见 --tag）
│  ├─ paddleocr-v6-det.onnx
│  ├─ paddleocr-v6-rec.onnx
│  └─ paddleocr-v6-dict.txt
├─ models-catalog.json        ← 提交到模型仓库 main 分支根目录
└─ UPLOAD.md                  ← 上传清单（含 sha256 与字节数）
```

## 为什么不需要改 ONNX

- 应用走 **ORT 后端**时，HuggingFace 的原始 ONNX（符号 batch + `value_info`）可以直接跑；
- 只有我们自己的 **tract 后端**才需要"batch 写死 / 去 value_info"的补丁，但 tract 0.20
  对 v6 rec 有已知数值 bug（见 `D:\test\_probe\out\b1_findings.md`），所以 v6 只面向 ORT 用户；
- 依赖中心升级前会跑**自检推理**，没有 ORT 的机器升级会被拦下并保持旧版本 —— 不会把 OCR 搞坏。

## 自检做了什么

1. 用仓库里的 `smoke_text.png`（"测试文本 ABC"）跑一次 det，要求检出区域（>0.05% 像素）；
2. 用同一张图跑一次 rec，要求至少识别出一个字符；
3. 通过后才生成清单。

无 `onnxruntime` / `Pillow` 时自检自动跳过（只做 sha256 与结构校验），建议发布前在本机装好再跑。

## 发布后的验证

1. 模型仓库 `main` 分支放好 `models-catalog.json`；
2. 应用：依赖中心 →「检查模型更新」→ 应看到"可升级到 v6-tiny-1 · 6MB"；
3. 点「升级」→ 下载校验 → 自检 → 切换（日志 `[DEPS] paddleocr 已升级到 v6-tiny-1（回退可用）`）；
4. 失败可点「回退到 <上一版>」。
