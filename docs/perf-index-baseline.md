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
| C2 | ~~大文件逐页 OCR 预算 600s → 120s~~ | `LARGE_SCAN_OCR_BUDGET` | ❌ 作废（2026-09-26）：总预算改为 §5.2 进度看门狗 |
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

## 5.1 二次复测（2026-09-26）：暴露并修复两个新问题

用同一批数据整库重建复测，`pdfimages page N timed out after 30s` 已 **236 → 0**、逐页 OCR 均值 **2.29s → ~0.5s**、`pdftoppm timed out` **10 → 0**（主病灶已除）。但复测又暴露两处：

| 问题 | 证据（复测日志/DB） | 根因 | 修复 |
|---|---|---|---|
| 整篇 `pdfimages` 120s 超时对**大卷宗**太短 | `pdfimages timed out after 120s` **37 次** → `per-page OCR loop` **92 次**（如 `卷七十八.pdf` 217 页 / 90MB，导出约 200MB 图片 >120s） | 固定 120s 未随工作量增长 | 见 §5.2：改为**进度看门狗**（不再用固定/缩放墙钟） |
| **SQLite 连接池**被 OCR 长期占用而耗尽 | `DB conn: timed out waiting for connection` **702 次**；一批 250 文件 **0 成功 250 失败**；DB `failed=350` | Phase-1 在**整个提取（含数分钟 OCR）期间持有连接**（`indexer.rs:425`），池 `max_size=12` == Phase-1 并发 12，扫描器/监听/回填/UI 无余量 → 10s 超时 | `db/mod.rs` 池 `max_size 12→24`、`connection_timeout 10s→30s` |

> 连接池修复的验证：`cargo check` 0 错误；`semgrep --severity ERROR` 0 findings。整库墙钟需重启后重跑回填。

## 5.2 超时改为进度看门狗（2026-09-26 定稿，机器无关）

**动机**：`clamp(30+页数×1.5s, …)` 这类公式是从**一次带负载的实测**推的（整篇 `pdfimages` 与 11 路 OCR 并发抢 CPU/IO），换台更慢的机器系数全变；方向偏紧会**误杀**并回退到慢的逐页路径。超时语义应是"卡死看门狗"，不是"性能 SLA"。

**新机制**：

| 阶段 | 进度定义 | 判定卡死 |
|---|---|---|
| 整篇 `pdfimages` | 输出目录 `img*` 的 (文件数, 总字节) 增长 | 连续 `stall` 秒零增长 → kill |
| 逐页 OCR 循环 | 每页返回（有/无文本）即算进展 | 单页耗时 ≥ `stall` → 停机 |

- `stall` 来源：环境变量 `LINK_SEARCHER_OCR_STALL_SECS` > 设置 `ocr_stall_timeout_secs` > 默认 **120s**；**`0` = 关闭看门狗**。
- 不再有总时长上限 → 慢机器只要持续推进就能跑完；慢 ≠ 卡死。
- 设置页「索引」标签可调（秒，步进 30，0=关闭）；i18n 四语言。
- 验证：`cargo test --lib extractor::pdf` **46 passed**；`cargo check` 0 错误；`npx tsc` 0 错误；`semgrep --severity ERROR` 0 findings。

## 6. 相关代码 / 常量

- 常量：`MIN_PAGE_IMAGE_AREA=100_000`、`LARGE_SCAN_PAGE_THRESHOLD=20`
- 看门狗：`DEFAULT_OCR_STALL_TIMEOUT=120s` + `global_ocr_stall_timeout()`（`extractor/pdf.rs`）；`output_progress()` / `clear_dir()` / `run_pdfimages_doc(.., stall)` / 逐页循环（`extractor/pdf/ocr.rs`）；设置 `ocr_stall_timeout_secs`
- 连接池：`db/mod.rs` `max_size=24`、`connection_timeout=30s`（2026-09-26 由 12 / 10s 调大）
- `extractor/pdf/scan.rs`：`image_list_info()`、`is_image_based_scan()`、`full_page_image_pages()`
- `extractor/pdf/poppler.rs`：`pdf_longest_side_pt()`
- `extractor/pdf/ocr.rs`：`run_pdf_ocr_pipeline()`、`try_ocr_fallback(.., allow_whole_doc_pdftoppm)`、`ocr_pdf_via_pdfimages()`、`run_pdfimages_doc()`、`group_images_by_page()`
- `extractor/pdf.rs`：`extract_with_lang()`（扫描分支先试 `pdftotext`）
- 尚未做（后续可选）：C3 逐页连续 3 页失败即中止（C2「逐页预算 600s→120s」作废：总预算已被 §5.2 进度看门狗取代）

## 7. 整库重跑结果（2026-09-26 15:06 会话，新二进制）

会话：`15:06:30 [STARTUP] 启动扫描` → `15:17:51 [STARTUP] 启动扫描完成`。

扫描汇总（日志原文）：`7791 files, 786 indexed, 0 moved, 7 errors in 672186ms`。

> ⚠️ 说明：本轮**绝大部分文件是去重命中**（`[INDEX] 去重 … 复用 md5` 共 **1659** 次，沿用上一轮已提取内容），因此本轮墙钟不代表"冷启动全量提取"；巨型卷宗本轮多被去重跳过，未再触发整篇 `pdfimages`。指标用于验证**新机制不再误杀/不再耗尽连接**。

| 指标 | Before（09-25 基线） | 复测（09-26 旧超时版） | **After（09-26 看门狗版）** |
|---|---|---|---|
| 整轮墙钟 | 1–2 h（预计） | 7.5k 秒到 7200/7791 | **672s（≈11.2 min）**（含 1659 去重） |
| `pdfimages page N timed out after 30s` | 236 | 0 | **0** |
| `pdfimages timed out after 120s` / `stalled` | 0 | 37 | **0** |
| `pdftoppm timed out after 120s` | 10 | 0 | **2**（小文档整篇渲染，属正常兜底） |
| `per-page OCR loop` | 34 | 92 | **9**（`stalled=true` **0**，看门狗未误杀） |
| `DB conn: timed out waiting for connection` | — | **702** | **0** |
| 单批最差（250 文件） | — | **0 成功 / 250 失败** | **248 成功 / 2 失败** |
| 本轮失败文件 | — | 350 | **7**（均真损坏：malformed FIB、invalid PDF trailer、加密文档、ToUnicode CMap 解析失败） |
| 单页 OCR 耗时（抽样） | 均值 2.29s | ~0.52s | **1.5–2.5s**（pdftoppm 逐页，小文件） |

**结论**：进度看门狗（不再有墙钟误杀，`stalled=true`=0）+ 连接池扩容（`DB conn: timed out` 702→0）两项修复在整库跑通；剩余 7 个失败为**真实损坏文件**，非工程缺陷。

## 7.1 冷启动全量重跑（2026-09-26 16:02 会话）

用 App「索引状态 → 重建索引」（`rebuild_index`，内部 `clear_index_tables` 清空 `file_tracking/content_index/doc_embeddings/doc_summaries/chunk_embeddings` 后全量重扫，**不动设置/模型**）。

会话：`16:02:15 [SCAN] 开始扫描` → `18:00:42 [SCAN] 扫描完成`。

扫描汇总（日志原文）：`7791 files, 7733 indexed, 58 errors in 7107262ms`。

| 指标 | 冷启动全量（After 定稿） |
|---|---|
| 提取阶段墙钟 | **7107s ≈ 1h58m**（7791 文件全量真提取，无去重） |
| `pdfimages page N timed out after 30s` | **0** |
| `pdfimages timed out(120s)` / `stalled` | **0** |
| `pdftoppm timed out` | **0** |
| `stalled=true`（看门狗误杀） | **0**（`per-page OCR loop` **60** 次全部正常完成） |
| `DB conn: timed out waiting for connection` | **0** |
| 结果 | `7733 indexed / 58 errors`（失败均真损坏：坏 JPEG、非 OLE2 `.doc`、加密/无文本文档） |
| 大卷宗 | `卷七十七.pdf`（98~217 页级）走整篇 `pdfimages` 成功，OCR 21515 字，无超时 |

> 嵌入回填随后进行（`[AI] 回填开始: 6770 文件缺嵌入`），属另一阶段，不计入提取墙钟。

**冷启动结论**：看门狗 + 连接池两项修复在**全量冷启动**下零超时、零误杀、零连接耗尽；相对 09-25「1–2 小时且需多轮」的基线，本轮**一次跑完**。

### 7.1.1 已知问题：重建后索引目录交换失败（Windows，待修）

```
18:00:42 ERROR [index] [SCAN] failed to swap index dir: 拒绝访问 (os error 5)
```
- 磁盘残留 3 个 `index.tmp-*`（含本次 118 分钟重建的成果），`.ls-index` 仍是**旧索引**。
- 根因：`commands/index.rs:447-463`，第 3 步已把内存 `IndexManager` 指向 `tmp_dir`，Tantivy 的 `Index`/`IndexReader` 在 Windows 上对 `tmp_dir` 持有打开的句柄 → `fs::rename(tmp_dir → .ls-index)` 被拒；旧目录→`index.old` 的 rename 亦用 `let _` 吞错。回滚后仍用旧索引，重启后本次重建的搜索索引丢失（SQLite 侧 `content_index`/向量是新的）。
- 影响：**重启后搜索结果回退到旧索引**；需释放句柄后再交换（见 CHANGELOG）。
- **修复（2026-09-26 三）**：交换前 `indexer.reset_writer()` + 用 `IndexManager::create_in_ram()` 占位顶掉指向 tmp/旧目录的 `Index`/Reader（释放 Windows 句柄）→ 再 rename；成功后 `open_or_create(.ls-index)` 装回。失败回滚旧目录并**清理孤儿 tmp 目录**；启动时清理残留 `index.tmp-*` / `index.old`（`lib.rs`）。


