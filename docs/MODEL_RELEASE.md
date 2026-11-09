# 发布模型 / 加速包（在线升级操作手册）

> 面向维护者。应用侧已经有「依赖中心 → 检查更新 → 升级 → 回退」的全链路，
> 这份文档只讲**发布端**：怎么造资产、传哪里、怎么验证、怎么回滚。

相关代码：

| 位置 | 作用 |
|---|---|
| `src-tauri/src/deps/catalog.rs` | 内置清单（首次安装用）+ ORT 加速包的 `FileSpec`（sha256 钉死） |
| `src-tauri/src/deps/remote.rs` | 远端清单 `models-catalog.json` 的拉取/过滤 |
| `src-tauri/src/deps/state.rs` | 版本目录与 `state.json`（current/previous） |
| `scripts/models/prepare_ppocrv6.py` | 生成 v6 资产 + 清单（带自检） |

---

## 0. 一次性准备

- 仓库：`Gray-Chan-sh/link-searcher-models`（可用环境变量 `LINK_SEARCHER_MODELS_GH` 覆盖）
- 清单位置：**`main` 分支根目录的 `models-catalog.json`**（raw 文件，不是 Release）
  `https://raw.githubusercontent.com/<repo>/main/models-catalog.json`
- 资产位置：GitHub **Releases**（按版本打 tag，例如 `models-v2`）
- 本机工具：Python 3.12（`pip install onnx onnxruntime pillow pyyaml`）、`gh`（已登录）、`Get-FileHash`

---

## 1. 发布新模型版本（以 PP-OCRv6 tiny 为例）

### 1.1 生成资产

```powershell
# 直接用 HuggingFace 原始模型（需网络）
python scripts/models/prepare_ppocrv6.py --out D:\dist\models-v2

# 离线：用本地已下载的模型目录（需含 det.onnx / rec.onnx / inference.yml）
python scripts/models/prepare_ppocrv6.py --out D:\dist\models-v2 `
  --from-dir D:\test\_probe\models\v6src `
  --version v6-tiny-1 --tag models-v2
```

产物：

```
D:\dist\models-v2\
├─ files\paddleocr-v6-det.onnx
├─ files\paddleocr-v6-rec.onnx
├─ files\paddleocr-v6-dict.txt
├─ models-catalog.json      ← 提交到模型仓库 main
└─ UPLOAD.md                ← 每个文件的 sha256 与字节数
```

脚本自带**自检**：用 `smoke_text.png` 跑 det（必须检出区域）+ rec（必须至少 1 个字），
不通过就拒绝生成清单。没装 `onnxruntime` / `Pillow` 时会跳过自检（不建议）。

> **不要给 v6 打 ONNX 补丁**。HF 原始模型（符号 batch + value_info）在 ORT 下可直接跑；
> 只有 tract 需要补丁，而 tract 0.20 对 v6 rec 有已知数值 bug，所以 v6 只面向 ORT 用户，
> 升级时的自检会把没装 ORT 的机器拦下并保持旧版本。

### 1.2 上传资产

```powershell
gh release create models-v2 --repo Gray-Chan-sh/link-searcher-models `
  --title "models-v2" --notes "PP-OCRv6 tiny (det/rec/dict)"
gh release upload models-v2 D:\dist\models-v2\files\* --repo Gray-Chan-sh/link-searcher-models
```

### 1.3 提交清单

把 `D:\dist\models-v2\models-catalog.json` 放到模型仓库 `main` 分支根目录并提交。
格式示例见 `docs/models-catalog.example.json`（字段含义）：

| 字段 | 说明 |
|---|---|
| `schema` | 固定 `1`；不匹配的客户端会整份忽略 |
| `deps.<id>.version` | 版本标识，同时是 `versions/<version>/` 目录名 |
| `deps.<id>.tag` | 资产所在 Release tag |
| `min_app_version` | 低于此版本的客户端不提示 |
| `files[].remote` | Release 里的文件名 |
| `files[].local` | 落地文件名（`det.onnx` / `rec.onnx` / `dict.txt`） |
| `files[].sha256` | 小写 hex；下载层逐文件校验，错了直接拒绝 |

---

## 2. 发布 ONNX Runtime 加速包（换版本时）

加速包的摘要**写死在 `catalog.rs`**，所以换 ORT 版本必须同步改代码（这是有意的：宁可让客户端拒绝，
也不接受来路不明的动态库）。

1）拿到官方 `onnxruntime.dll`（Windows x64，版本需与 `ort` crate 绑定的 API 对齐：当前 **1.31.x**）

2）算摘要：

```powershell
Get-FileHash .\onnxruntime.dll -Algorithm SHA256
(Get-Item .\onnxruntime.dll).Length
```

3）改 `src-tauri/src/deps/catalog.rs` 的 `onnxruntime()`：

```rust
files: &[FileSpec {
    remote: "onnxruntime-win-x64-1.31.1.dll",   // ← Release 里的文件名（保持原样上传）
    local: "onnxruntime.dll",
    sha256: "<上一步的 hash 小写>",
}],
```

4）上传到**与模型同一个 Release tag**（默认 `models-v1`）：

```powershell
gh release upload models-v1 .\onnxruntime.dll#onnxruntime-win-x64-1.31.1.dll `
  --repo Gray-Chan-sh/link-searcher-models --clobber
```

5）提交代码改动（`Cargo.toml` 里的 `ort` 版本若也变了，一并提交）并走正常发版流程。

> 当前钉住的摘要（1.31.1）：`e45f61e50d8650885452be538a00bb14a715e46a1bc3691000ce947e89ed0306`，
> 19,288,376 字节。

---

## 3. 发布后验证（必做）

1. 装/确认「ONNX Runtime 加速包」已就绪（依赖中心显示 ✓；日志 `[OCR] 后端 = ONNX Runtime`）
2. 依赖中心 →「检查模型更新」→ 应显示 `1 个模型可升级`，并给出新版本号与体积
3. 点「升级」→ 观察日志：

```
[DEPS] paddleocr: 3 file(s) to fetch (~6.0 MB total)
[DEPS] paddleocr 已升级到 v6-tiny-1（回退可用）
[OCR] 已丢弃旧引擎池，下一轮推理将重新加载模型
```

4. 随便找一份扫描件索引一页，确认有文字产出；`依赖中心` 里应显示当前版本号
5. 断网再点一次「检查更新」→ 应静默提示"没有取到更新清单"，不影响使用

---

## 4. 回滚

| 场景 | 做法 |
|---|---|
| 新模型有问题（应用侧） | 依赖中心 →「回退到 <上一版>」（`state.json` 的 `previous` 切回，立即生效） |
| 清单本身有问题（发布侧） | 修 `main` 分支的 `models-catalog.json`（或把 `version` 改回旧值）；已升级的客户端不受影响，只有再次检查时才看到 |
| 加速包有问题 | 依赖中心可停用：把 `config.json` 的 `ocr_backend` 改 `"tract"`；或删除 `<数据目录>/runtime/onnxruntime/` 下的 dll |

磁盘上只保留 **当前 + 上一版**（`models/paddleocr/versions/<ver>/`），更旧的目录会在升级成功后被清理。

---

## 5. 常见问题

| 现象 | 原因 / 处理 |
|---|---|
| 升级报“新模型自检没有识别出任何文字” | 模型不能用（或在 tract 上跑 v6）；检查是否装了 ORT、模型是否被改坏 |
| 依赖中心显示 ✓ 但 OCR 仍慢 | 看启动日志的后端；若为 tract，说明 dll 名字/位置不对或版本不符（需 1.31.x） |
| `sha256 校验失败（源 …）` | 发布端换了文件但没更新清单里的摘要；重新生成清单 |
| 清单拉不到 | raw 分支被墙或仓库私有；可设 `LINK_SEARCHER_MODELS_CATALOG` 指向可用地址 |
| 升级后想立刻验证 | 不用重启：升级流程会丢弃旧引擎池，下一次 OCR 即用新模型 |
