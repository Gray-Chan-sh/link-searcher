# 发布模型资产（在线升级用）

`prepare_ppocrv6.py`：生成 `models-catalog.json`（应用从模型仓库 `main` 分支的 raw 文件拉取）。

**策略：全直连上游，零自托管**——模型来自 HuggingFace（经 hf-mirror.com），ORT 来自微软
Release；我们只维护这份清单（版本 + sha256 + URL）。

## 用法

```bash
python scripts/models/prepare_ppocrv6.py --out D:\dist\catalog
# 只生成清单、跳过自检
python scripts/models/prepare_ppocrv6.py --out D:\dist\catalog --no-verify
# 指定 revision（默认自动取 HF 当前 commit sha）
python scripts/models/prepare_ppocrv6.py --out D:\dist\catalog --revision-det <sha> --revision-rec <sha>
```

产物：

```
D:\dist\catalog\
├─ models-catalog.json   ← 提交到模型仓库 main 分支根目录
└─ cache\                ← 下载过的上游文件（仅本地缓存，不提交）
```

**不需要** `gh release`、不需要上传任何模型文件。

## 清单里三个文件

| local | 来源 | 说明 |
|---|---|---|
| `det.onnx` | HF `PP-OCRv6_tiny_det_onnx/resolve/<rev>/inference.onnx` | 检测模型 |
| `rec.onnx` | HF `PP-OCRv6_tiny_rec_onnx/resolve/<rev>/inference.onnx` | 识别模型 |
| `dict.txt` | 同 `inference.yml`，标 `"dict_from_yaml": true` | 上游没有独立字典，应用下载 yml 后自己抽 `character_dict` |

## 关键约束

- **必须 pin revision**（脚本自动取 commit sha）：`resolve/main` 会变，变了 sha256 就对不上。
- 清单里的 `sha256` 是**被下载原始文件**的摘要（yml 是 yml 的、不是抽出来的字典的）。
- v6 只面向 ORT 用户：tract 0.20 对 v6 rec 有已知数值 bug，升级时的自检会拦下没装 ORT 的机器。
- 上游地址在国内默认走 `hf-mirror.com`；`github.com` 的 URL 会自动套 ghfast/gh-proxy 镜像链。

## 发布后的验证

1. 模型仓库 `main` 放好 `models-catalog.json`；
2. 应用：依赖中心 →「检查模型更新」→ 应看到"可升级到 v6-tiny-1 · 6MB"；
3. 点「升级」→ 下载校验 → 自检 → 切换（日志 `[DEPS] paddleocr 已升级到 v6-tiny-1（回退可用）`）；
4. 失败可点「回退到 <上一版>」。

完整发布/回滚流程见 `docs/MODEL_RELEASE.md`。
