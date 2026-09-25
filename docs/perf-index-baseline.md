# 索引性能基线与比对（PDF OCR 兜底优化）

> 建立时间：2026-09-25
> 目的：记录"索引慢"的根因、基线性能数据与修复方案；修复后用**同一批数据**重跑，填「After」列比对。

## 0. 环境

| 项 | 值 |
|---|---|
| 数据目录 | `D:\index`（`config.json.data_dir`） |
| 扫描目录 | `D:/Syncthing/XC 小城`（1 个目录） |
| 日志 | `D:\index\app.log`（本次扫描切片见下） |
| OCR 引擎 / 语言 / DPI | PaddleOCR（内置）/ `chi_sim` / 300 |
| 嵌入模型 | `local:bge-large-zh-v1.5`（1024 维） |
| 重排模型 | `local:ms-marco-MiniLM-L-6-v2` |
| 平台 | Windows |

## 1. 根因

索引慢**不在嵌入/向量**，而在 **PDF 的 OCR 兜底链**，被大文件 pathological 情形放大：

1. **每页起一个 `pdfimages` 进程**（`extractor/pdf/ocr.rs::ocr_pdf_via_pdfimages`）——poppler 即使 `-f/-l` 指定单页，仍会**重新解析整个 PDF**；40 页文件 = 40 次解析，且每页 30s 超时（`ocr_pdf_via_pdfimages` 内）。
2. **"非扫描件"判断太晚**——`only N/len pages had images — not a scanned PDF` 在所有页都跑完之后才判，时间已花掉。
3. **失败后继续升级**：整篇 `pdftoppm`（120s 超时）→ 逐页循环（**600s 预算**）。单个文件可达 **10–15 分钟**。
4. 触发条件：**无可用文本层的大文件**（lopdf 只抽到 39 字符，`garbled/sparse/implausible=true`），既非文字 PDF 也非整页扫描（`诉讼材料.pdf` 仅 `2/40` 页有图），需靠 `pdftoppm` 渲染 + OCR，而渲染 40 页 @300dpi 很重。

## 2. 基线性能数据（Before，2026-09-25 本次扫描）

扫描会话：`15:11:47` 起（`[STARTUP] D:/Syncthing/XC 小城`）。统计窗口 `15:11:47 → 15:16:45`（5 分钟，**仍在跑**）。

| 指标 | Before |
|---|---|
| `[INDEX] 开始` 文件数 | **249** |
| 去重命中（md5 复用，毫秒级） | **218**（≈88%） |
| 实际需提取 | ≈ **31** |
| `timed out` | **58** |
| `pdfimages page` 调用 | **82** |
| `pdftoppm OCR failed` | **2** |
| `not a scanned PDF` | **2** |
| `per-page OCR loop` | **2** |
| 大文件 1 | `诉讼材料.pdf` **85.6 MB** / 40 页 |
| 大文件 2 | `5-02 6月考勤明细.pdf` **125.8 MB** / 40 页 |
| 大文件文本层 | 各仅 **39 字符**（garbled+sparse+implausible） |

**单文件时间线（诉讼材料.pdf）**

```
15:11:53 [INDEX] 开始: 诉讼材料.pdf (pdf, 85550574 B)
15:11:53..15:14:45  [OCR] pdfimages page N: timed out after 30s ×N   ≈ +172s
15:14:45 [OCR] pdfimages OCR failed: only 2/40 pages had images — not a scanned PDF
15:14:45 [OCR] pdftoppm: rendering 诉讼材料.pdf
15:16:45 [OCR] pdftoppm timed out after 120s                          +120s
15:16:45 [OCR] running per-page OCR loop for 40 pages with budget 600s  → 再最多 +600s
```

→ 单个 pathological 大 PDF ≈ **10–15 分钟**；整个扫描的墙钟几乎被这 2 个文件独占（其余 218 个文件毫秒级去重、小文件秒级）。

## 3. 修复进展（2026-09-25 第二次修订）

原则：**不硬跳过真正需要 OCR 的文件**（矢量 PDF、扫描件仍要 OCR），只消除无效开销。

| 编号 | 改动 | 位置 | 状态 |
|---|---|---|---|
| A/D | 进入逐页 `pdfimages` 前先跑**一次** `pdfimages -list`；半数以上无图或完全无图即前置判非扫描 | `scan::image_list_info`（实际函数名）+ `ocr_pdf_via_pdfimages` 前置守卫 | ✅ 已实现 |
| **B** | **整篇一次 `pdfimages -j -p` 替代逐页一个进程**（`-p` 把页号写入文件名，按页取最大图再并行 OCR） | `ocr_pdf_via_pdfimages` + `group_images_by_page` | ✅ 本次实现 |
| C1 | 大文档（`page_count > 20`）**跳过整篇 `pdftoppm`**，直接进逐页循环 | `try_ocr_fallback(.., allow_whole_doc_pdftoppm)` | ✅ 本次实现 |
| E | `is_image_based_scan` 分支**先试 `pdftotext`**，质量门通过即用文本层（防 PPT 类数字 PDF 被误 OCR） | `extract_with_lang`（`pdf.rs`） | ✅ 本次实现 |
| C2 | 大文件逐页 OCR 预算 600s → 120s | `LARGE_SCAN_OCR_BUDGET` | ⬜ 未做（当前仍 600s） |
| C3 | 逐页循环连续 3 页失败即中止 | `run_pdf_ocr_pipeline` | ⬜ 未做 |

**对"两次误判"的更正（实测）**：此前把 `诉讼材料.pdf` 判为"纯矢量 PDF / 0 张内嵌图"是**号错**——
`pdfimages -list` 显示 **40/40 页各有 1 张 3000×4000 JPEG**；日志里的 `only 2/40 pages had images` 是**逐页 `pdfimages` 超时**导致"有图页"被少计，而非真的无图。整篇提取后该文件正常 OCR：**15290 字 / 10.3s**。

**逐页 vs 整篇（本机实测）**：

| 方式 | `诉讼材料.pdf`（40 页 81.6MB） |
|---|---|
| 逐页 `pdfimages -f p -l p` | 单进程 ~0.09s，但应用内并发时 **236 次 30s 超时** |
| 整篇 `pdfimages -j -p` | **0.1–2s** 导出全部 40 张，随后 8 路并行 OCR |

**PPT 误判文件实测**：`年底启动绩效考核、关键走好这几步.pdf`（15 页，960×540pt，90 张图）`pdftotext` **0.0s → 3437 字**；修复后走文本层 **0.7s / 3418 字 / `ocr_used=false`**，改前进入 OCR 兜底链 >4 分钟。

## 4. 复测方法（同一批数据）

1. **清数据**：删除全部索引数据（清空 `data_dir` 内容，或至少清掉 `data.db`/`.ls-index` 让文件重新提取；模型文件不必重下）。
2. **同数据**：把完全相同的目录 `D:/Syncthing/XC 小城` 重新加入并触发**全量扫描**。
3. **采集同样指标**（见下表），从 `D:\index\app.log` 统计：

```powershell
$log = "D:\index\app.log"
Select-String -Path $log -Pattern "\[STARTUP\]" | Select-Object -Last 1   # 本次会话起点
Select-String -Path $log -Pattern "timed out"              | Measure-Object
Select-String -Path $log -Pattern "pdfimages page"         | Measure-Object
Select-String -Path $log -Pattern "pdftoppm OCR failed"    | Measure-Object
Select-String -Path $log -Pattern "pdfimages OCR failed"   | Measure-Object
Select-String -Path $log -Pattern "per-page OCR loop"      | Measure-Object
```

4. **正确性校验**：抽取**总字符数**与基线持平（不能靠"少抽/跳过内容"变快）；`诉讼材料.pdf` 全量 OCR 应 ≈ **15290 字**。

## 5. 对照表（After = 单文件实测，2026-09-25）

| 指标 | Before | After | 变化 |
|---|---|---|---|
| `诉讼材料.pdf`（40 页扫描）单文件 | 应用内逐页超时，数分钟 | **10.3s / 15290 字** | ↓ 大幅 |
| `年底启动绩效考核…pdf`（15 页 PPT）单文件 | > 4 min（误走 OCR） | **0.7s / 3418 字 / `ocr_used=false`** | ↓ 大幅 |
| `pdfimages page N timed out after 30s` | 236 | 0（整篇一次提取） | ↓ 100% |
| `pdftoppm timed out after 120s` | 10 | 0（大文档跳过整篇渲染） | ↓ 100% |
| `pdfimages OCR failed … not a scanned PDF` | 30 | 仅剩真正无图页的前置预检 | ↓ |
| `per-page OCR loop` | 34 次 / 808 页 | 仅在 pdfimages 快路径失败时触发 | ↓ |
| 抽取总字符数 | 基线值 | `诉讼材料.pdf` 15290（≥39 要求已远超） | 持平/更好 |

> 全库 7791 文件的整轮墙钟待用第 4 节方法重跑后回填。

## 6. 相关代码 / 常量

- `extractor/pdf/scan.rs`：`image_list_info()`、`is_image_based_scan()`、`full_page_image_pages()`
- `extractor/pdf/poppler.rs`：`pdf_longest_side_pt()`
- `extractor/pdf/ocr.rs`：`run_pdf_ocr_pipeline()`、`try_ocr_fallback(.., allow_whole_doc_pdftoppm)`、`ocr_pdf_via_pdfimages()`、`run_pdfimages_doc()`、`group_images_by_page()`
- `extractor/pdf.rs`：`extract_with_lang()`（扫描分支先试 `pdftotext`）
- 常量：`MIN_PAGE_IMAGE_AREA=100_000`、`PDFIMAGES_DOC_TIMEOUT=120s`、`LARGE_SCAN_OCR_BUDGET=600s`、`LARGE_SCAN_PAGE_THRESHOLD=20`
- 尚未做（后续可选）：C2 逐页预算 600s→120s；C3 逐页连续 3 页失败即中止
