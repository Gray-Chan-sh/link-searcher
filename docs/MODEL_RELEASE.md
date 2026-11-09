# 发布模型 / 加速包（在线升级操作手册）

> 面向维护者。应用侧已有「依赖中心 → 检查更新 → 升级 → 回退」全链路。
> **策略：全直连上游，我们不托管任何模型/运行时文件**；发布 = 提交一份清单。

相关代码：

| 位置 | 作用 |
|---|---|
| `src-tauri/src/deps/catalog.rs` | 内置清单（首装用）+ ORT 加速包（直连微软官方 zip） |
| `src-tauri/src/deps/remote.rs` | 远端清单 `models-catalog.json` 的拉取/过滤（支持绝对 URL、解压、抽字典） |
| `src-tauri/src/deps/download.rs` | 下载 + sha256 校验 + `PostStep`（zip 取成员 / yml 抽字典） |
| `src-tauri/src/deps/state.rs` | 版本目录与 `state.json`（current/previous） |
| `scripts/models/prepare_ppocrv6.py` | 生成清单（pin 上游 revision + 自检） |

---

## 0. 一次性准备

- 清单托管：模型仓库 **`main` 分支根目录的 `models-catalog.json`**
  `https://raw.githubusercontent.com/<repo>/main/models-catalog.json`
  （仓库默认 `Gray-Chan-sh/link-searcher-models`，可用 `LINK_SEARCHER_MODELS_GH` 覆盖）
- 本机工具：Python 3.12（`pip install onnxruntime pillow`）、`git`
- **不需要** `gh release`、不需要上传任何资产

---

## 1. 发布新模型版本（以 PP-OCRv6 tiny 为例）

### 1.1 生成清单

```powershell
python scripts/models/prepare_ppocrv6.py --out D:\dist\catalog
```

它会：

1. 调 HF API 取两个仓库的 **commit revision**（`2ba1506…` / `2612ab3…`）并 pin 到 URL；
2. 从 `hf-mirror.com` 拉 `inference.onnx`（det/rec）与 `inference.yml`（rec 仓库），算 sha256；
3. 用仓库里的 `smoke_text.png` 跑一次自检（det 要检出区域、rec 要出字）；
4. 生成 `models-catalog.json`。

字典怎么来：上游没有独立字典文件，它在 `inference.yml` 的 `character_dict` 里。
清单里这一项标 `"dict_from_yaml": true`，**应用下载 yml 后自己抽成 `dict.txt`**。

> 为什么必须 pin revision：`resolve/main` 随时可能被上游改，一旦变了我们钉的 sha256
> 就对不上，用户升级会失败。HF 的 commit sha 是不可变的。

### 1.2 提交清单

```powershell
# 在模型仓库里
git add models-catalog.json && git commit -m "catalog: v6-tiny-1" && git push
```

无需上传模型文件——它们留在 HuggingFace。

### 1.3 清单字段

| 字段 | 说明 |
|---|---|
| `schema` | 固定 `1`；不匹配的客户端整份忽略 |
| `version` | 版本标识，同时是 `versions/<version>/` 目录名 |
| `min_app_version` | 低于此版本的客户端不提示 |
| `files[].remote` | **绝对 URL**（HF 或 Releases 都行；`github.com` 会自动套镜像链） |
| `files[].local` | 落地文件名（`det.onnx` / `rec.onnx` / `dict.txt`） |
| `files[].sha256` | 被下载**原始文件**的摘要（解压/抽字典前） |
| `files[].extract` | 可选：zip 成员后缀（如 `lib/onnxruntime.dll`） |
| `files[].dict_from_yaml` | 可选：从 PaddleOCR 的 `inference.yml` 抽字典 |

---

## 2. ONNX Runtime 加速包（换版本时）

内置清单里的 `onnxruntime()` **直连微软官方发布包**（`onnxruntime-win-x64-<ver>.zip`），
应用下载整包后按后缀取出 `lib/onnxruntime.dll` 放到 `<数据目录>/runtime/onnxruntime/`。

换版本步骤：

1. 到 <https://github.com/microsoft/onnxruntime/releases> 找目标版本（**需与 `ort` crate 绑定的
   API 对齐**，当前 1.31.x）的 `onnxruntime-win-x64-<ver>.zip`
2. 算摘要与大小：

```powershell
Get-FileHash .\onnxruntime-win-x64-1.31.1.zip -Algorithm SHA256
(Get-Item .\onnxruntime-win-x64-1.31.1.zip).Length
```

3. 改 `src-tauri/src/deps/catalog.rs` 的 `onnxruntime()`：`remote` 换成新版本 zip 的 URL、
   `sha256` / `size_bytes` 同步；必要时更新 `Cargo.toml` 的 `ort` 版本
4. 正常发版

> 当前钉住（1.31.1）：`5869e26bb25314375f2d5ef1877e3ea06b1543d6c710e6e4a1ec2f7177ab4b2c`，
> 82,929,944 字节（整包 ~79MB，取出的 dll ~18MB）。

> 注意体积：这是**上游整包**，比单发 dll 大很多。若在意流量，可改回"自托管 18MB dll"
> （把 `remote` 指向我们 Release 的 dll，并删掉 `post_step_for` 里的 `ZipMember` 分支）。

---

## 3. 发布后验证（必做）

1. 依赖中心 →「检查模型更新」→ 应显示 `1 个模型可升级`，并给出新版本号与体积
2. 点「升级」→ 日志：

```
[DEPS] det.onnx ← GitHub Releases (https://hf-mirror.com/…/inference.onnx)
[DEPS] paddleocr 已升级到 v6-tiny-1（回退可用）
[OCR] 已丢弃旧引擎池，下一轮推理将重新加载模型
```

3. 索引一页扫描件确认有文字；依赖中心显示当前版本号
4. 断网点「检查更新」→ 静默提示"没有取到更新清单"，不影响使用

---

## 4. 回滚

| 场景 | 做法 |
|---|---|
| 新模型有问题（应用侧） | 依赖中心 →「回退到 <上一版>」（`state.json` 的 `previous` 切回，立即生效） |
| 清单有问题（发布侧） | 修 `main` 分支的 `models-catalog.json`（或把 `version` 改回旧值） |
| 加速包有问题 | `config.json` 的 `ocr_backend` 改 `"tract"`；或删掉 `<数据目录>/runtime/onnxruntime/` 下的 dll |

磁盘只保留 **当前 + 上一版**（`models/paddleocr/versions/<ver>/`），更旧的会在升级成功后清理。

---

## 5. 常见问题

| 现象 | 原因 / 处理 |
|---|---|
| 升级报"新模型自检没有识别出任何文字" | 模型不能用（或没装 ORT，v6 在 tract 上不可用）；旧版本保持不变 |
| 升级报 `sha256 校验失败` | 上游文件变了 / 清单没跟着更新；重新跑脚本生成清单（pin 新 revision） |
| 依赖中心显示 ✓ 但 OCR 仍慢 | 看启动日志后端；若是 tract，dll 位置不对或版本与 `ort` API 不符（需 1.31.x） |
| 清单拉不到 | raw 被墙或仓库私有；可设 `LINK_SEARCHER_MODELS_CATALOG` 指向可用地址 |
| 想换回自托管 | 把 `files[].remote` 改成我们 Release 的 URL 即可，下载层对两种来源一视同仁 |
