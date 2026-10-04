# 分图 17b · PDF 提取与 OCR 深度路径

> 🖼️ **交互式分图**：[打开 17b-pdf-ocr-path.html](17b-pdf-ocr-path.html)
> 📁 **本次任务在图中的位置**：主图 [17-file-lifecycle.html](17-file-lifecycle.html)「Phase 1 提取」里的 **PDF 高级路径**节点在此展开。
> 🧩 **typed 源**：[specs/17b-pdf-ocr-path.json](specs/17b-pdf-ocr-path.json)

本文是分图 17b 的**配套文字说明**，面向开发者，逐一解释图里每条路径、每个决策点、每个参数/阈值的含义与源码位置。目标是让你在不开图的情况下也能读懂 PDF 提取与扫描件 OCR 的完整逻辑。

---

## 0. 为什么 PDF 需要一张专门的图

其他格式（txt / Office / 图片 / 音频 / 压缩包）的提取基本是「一种格式 → 一个提取器 → 文本」，一条直线。**PDF 不是**：

- 它可能**有干净的文本层**（数字生成的 PDF）；
- 可能**有文本层但不可信**（水印、乱码、字被拆成一行一字）；
- 可能是**纯扫描件**（整页都是图，没有文字）；
- 可能**旋转了 90°/270°**（复印机横向进纸）；
- 可能**连解析器都打不开**（损坏、加密、非常规 xref）。

这些情况需要**逐级兜底判断**，而且判断错方向（该 OCR 没 OCR、不该 OCR 却 OCR）会导致「内容全丢」或「白白跑几分钟 OCR」。所以 17b 把这条最复杂的链路完整画出来。

**入口函数**：`src-tauri/src/extractor/pdf.rs`

| 函数 | 作用 |
|------|------|
| `PdfExtractor::extract_with_lang(path, lang, engine)` | 主逻辑，返回 `(文本, ocr_used)` |
| `PdfExtractor::extract_with_meta(...)` | 包一层，附带 `page_count` 等 `ExtractMeta` 供质量评分用 |
| `extractor::extract_text_with_meta` | 格式路由里 `"pdf"` 分支调用它 |

---

## 1. ① lopdf 解析（第一道，也是最理想的一道）

**节点**：① lopdf 解析
**源码**：`pdf.rs` → `PdfExtractor::extract_with_lang`

- 用 `lopdf::Document::load(path)` 打开，并用 `std::panic::catch_unwind` **包住**——有些畸形 PDF 会让 lopdf 直接 panic，必须防住，否则整批索引会崩。
- 三种结果，走不同支路：

| 结果 | 走向 |
|------|------|
| `Ok(doc)` 且能取到页 | 进入「逐页抽取文本层」（② 之后的主干） |
| `Err(e)`（parse 失败） | → **② pdftotext 兜底** |
| `panic` | → **② pdftotext 兜底**（同样） |

> 图上的边 `lopdf →（parse 失败 / panic）→ pdftotext` 就是这两种失败。

---

## 2. ②③ 文本层多级兜底（失败才走）

### ② pdftotext 兜底

**节点**：② pdftotext 兜底
**源码**：`pdf.rs` → `try_pdftotext_extract`

调用 poppler 的 `pdftotext path -`（超时 120s）。拿到输出后做**三重否决**，任一命中就认为不可用、继续往下兜：

1. 文本长度 `< 100` → 丢弃；
2. 按换页符 `\x0c` 切页后，若 ≥2 页且 `is_watermark_text` 判为水印 → 丢弃；
3. `is_repetitive(text)` 判为高度重复 → 丢弃。

通过则直接返回 `(text, ocr_used=false)`，**跳过后续所有 OCR**（图上的「恢复文本」边）。

### ③ anydoc 兜底

**节点**：③ anydoc 兜底
**源码**：`pdf.rs`（`anydoc::to_markdown`）

当 lopdf 与 pdftotext **都**失败时，再用 `anydoc` 试一把（它能处理 Quartz / CFF 等特殊 PDF）。`> 100` 字符才算数，否则视为空、继续往下。

### ③ 之后：进入图像 OCR

若 ①②③ 全部拿不到可信文本，就进入**扫描件 OCR 管线**（⑥）。这是图上 `anydoc →（再失败）→ OCR 管线` 那条粗边。

> **页树为空的特例**：`doc.get_pages()` 为空时（异常 / 加密 / 非标准 xref），不静默返回空串，而是先试 pdftotext 恢复（且要求 `!is_garbled_text`、非空），再不行才 OCR。

---

## 3. 主干：逐页抽取 → ④ 文本层质量判定

**源码**：`pdf.rs` 主干 + `pdf/quality.rs`

lopdf 解析成功后，逐页调用 `doc.extract_text(&[page_num])`（同样每页 `catch_unwind`，单页失败只记空串、不中断），拼成 `merged`。随后做**文档级** 5 项判定：

| 信号 | 函数 | 判定标准（源码阈值） |
|------|------|---------------------|
| **sparse**（过稀） | `is_sparse_text_layer` | 非空白字符数 `< 50 × 页数`（真实正文每页远多于 50 字） |
| **garbled**（乱码） | `is_garbled_text` | 可疑字符（`\u{FFFD}` / 控制字符）占比 `> 30%`；或非空白字符占比 `< 5%` |
| **watermark**（水印） | `is_watermark_text` | 逐页前缀「归一化」（去 hex 校验码/日期/URL/空白）后，相邻页相同比例 `> 80%` |
| **repetitive**（重复） | `is_repetitive` | 非空行去重后，重复行占比 `> 60%`（且总长 ≥100、行数 ≥3） |
| **implausible**（不可信） | `is_implausible_text_layer` | 每页非空白字符 `> 20000`（物理不可能）；或命中 `LowPrintable` 质量标志 |

> **doc_clean** = `merged.len() > 100 且 五项全否`。只有 `doc_clean` 才认为文本层可信。

### 3.1 分支 A：doc_clean 不成立 → 尝试 pdftotext 恢复

`pdf.rs`：若文档级文本层不干净，且 `try_pdftotext_extract` 有结果，用 `prefer_recovered_text(lopdf_text, recovered, page_count)` 判断是否采用：

```
recovered 非乱码 && 非不可信 && 非过稀 && 字数 > lopdf 的字数
```

满足则返回恢复文本，**不 OCR**。

### 3.2 分支 B：doc_clean 成立 → 逐页精修（混合 PDF）

即使整篇文本层看起来健康，个别页仍可能是扫描件/空页。此时用 `pdf_inspector::classify_pdf_mem` 的**逐页**信息判断哪些页需要 OCR：

- `pages_needing_ocr` 非空且占过半页，或整篇 sparse → 视为 Scanned/ImageBased → **绕过文本层整篇 OCR**；
- 否则逐页判 `page_needs_ocr(page_text, page_has_images)`：

| 页级条件 | 函数 |
|----------|------|
| 该页文本过稀（`is_sparse_text_layer(text, 1)`） | → 需 OCR |
| 该页乱码（`is_garbled_text`） | → 需 OCR |
| 有图且文本高度重复（`is_repetitive`） | → 需 OCR |

- `ocr_count == 0` → 直接返回文本，**不 OCR**；
- `0 < ocr_count < 页数` → **混合 PDF**：只对需 OCR 的页调 `ocr_single_pdf_page`，再用 `merge_page_texts` 把 OCR 结果**拼接进**原文本层（逐页 splice）；若个别页 OCR 失败，回退整篇 OCR。

### 3.3 关键细节：字体缺 /Encoding

**源码**：`pdf/quality.rs::has_unparseable_font_encoding`

检测「页内某字体没有 Name 值的 `/Encoding`、但有 `/ToUnicode`」。这种情况 `doc.extract_text` 会**静默丢字**（输出看着干净但其实缺字），所以此时优先用 pdftotext 恢复（要求恢复文本非乱码、非不可信、非过稀）。

> 中文 PDF（macOS Quartz / WPS 子集 TrueType，如 `AAAAAC+STSongti-SC-Regular`）常见此问题。

---

## 4. ⑤ 扫描件与旋转检测

**节点**：⑤ 扫描件 / 旋转检测
**源码**：`pdf/scan.rs`

| 检测 | 函数 | 标准 |
|------|------|------|
| **整页图片 = 扫描件** | `is_image_based_scan` | 跑 `pdfimages -list`，用 `full_page_image_pages` 找出「图片物理尺寸 ≥ 页面尺寸 80%（覆盖率高）且过半页带图」的文档 |
| **页面旋转 90°/270°** | `pages_rotated` | 遍历页 `/Rotate`（沿 `Parent` 继承），任一页为 90 或 270 即为真 |
| **pdf_inspector 分类** | `classify_pdf_mem` | 判 `Scanned` / `ImageBased`，或过半页需 OCR |

**为什么旋转要单独判？** `pdfimages` 会**原样复制**页内嵌入图、**不应用 `/Rotate`**，所以旋转页喂给 OCR 会得到「一字一行」的乱码。而 `pdftoppm` 会把旋转**烘焙进**渲染结果。因此检测到旋转时，OCR 管线**跳过 pdfimages，直接用 pdftoppm**。

| 情况 | 走向 |
|------|------|
| 扫描件 / 稀疏 / 乱码 | → **⑥ OCR 管线** |
| 文本层可用 | → 直接用文本（回到主图清洗） |

> 覆盖率阈值为 `FULL_PAGE_IMAGE_COVERAGE = 0.8`（`scan.rs`）。当 `pdfimages -list` 的 ppi 列为 0（IntSig/Foxit 常见）时，退化为「按 1px = 1pt 粗估」，**宁可误判为扫描件**（多跑一次 OCR）也不漏掉整份扫描文档。

---

## 5. ⑥ PDF OCR 管线

**节点**：⑥ PDF OCR 管线 / pdfimages 提取 / OCR 结果质检
**源码**：`pdf/ocr.rs` → `run_pdf_ocr_pipeline` → `try_ocr_fallback`

### 5.1 整篇 vs 分页的取舍

常量 `LARGE_SCAN_PAGE_THRESHOLD = 20`（`pdf.rs`）：

- **页数 ≤ 20**：先试**整篇快速路径**（一次渲染所有页，OCR 更快）；
- **页数 > 20**：**跳过整篇 pdftoppm**（渲染全部页动辄几分钟、必踩 120s 超时），直接进分页循环。

### 5.2 快速路径：pdfimages 优先，pdftoppm 兜底

**源码**：`try_ocr_fallback`

1. 若 `pdfimages` 可用且**未跳过**（旋转页才跳过），跑 `ocr_pdf_via_pdfimages`：
   - **整篇一次 `pdfimages -p`**（`-p` 把页码写进文件名），而不是「每页重新解析整个 PDF」；
   - **格式选择**：`pdfimages -list` 显示所有图都是 JPEG → 用 `-j`（原生拷贝，~0.1s/页）；否则用 `-png`；
   - 若 `-j` 一张图都没抽到（CCITT/JBIG2 等），清空目录后用 `-png` 重试一次；
   - 按页分组，每页取**面积最大的图**；面积 `< MIN_PAGE_IMAGE_AREA = 100_000 px²`（图标/logo）跳过；
   - 对每页图并行 OCR。
   - **进度看门狗**：`run_pdfimages_doc` 只在「一段时间没有新输出文件」时才杀进程（不是整篇硬预算）。
2. pdfimages 结果若「疑似可用」则直接用；若 `ocr_text_is_unusable` **判废**，记录下来转 pdftoppm。
3. 整篇 pdftoppm（页数 ≤20 时才允许）——`pdftoppm -png -scale-to <longest-side>`，120s 超时。
4. 若 pdfimages 与 pdftoppm 都失败，**保留那份被判可疑的 pdfimages 文本**（宁全勿丢）。

### 5.3 判废：`ocr_text_is_unusable`

**源码**：`pdf/ocr.rs`

旧的判据只看 `len > 100`，会把旋转页乱码（每个字一行、很长）放过。现在改成三重判废，任一命中即废：

1. `is_garbled_text(text)`；
2. **换行比正文还密**：`换行数 × 2 ≥ 非空白字符数`（「一字一行」的典型形态）；
3. 复用质量评分，命中 `LowPrintable` / `LowLexicon` / `HighFffd` 任一标志。

### 5.4 分页循环（大文档）

**源码**：`run_pdf_ocr_pipeline` 分页分支

- 逐页 `ocr_single_pdf_page(path, page_num, dpi, ...)`；
- 每页：`pdftoppm -png -scale-to <safe> -f <n> -l <n>`（**单页 30s 超时**）→ OCR 该页图 → 去空白，空则返回 `None`；
- **中止条件**：前 5 页全无文本 → 认为整份不可 OCR，跳出；
- **进度看门狗**：若**单页**耗时 ≥ 卡死阈值则停止（见 §6.4）。

---

## 6. ⑦ OCR 引擎 → 逐页拼接 → 输出

### 6.1 渲染尺寸（longest-side）

**源码**：`pdf/ocr.rs::render_longest_side`

- 目标像素 = `dpi / 72 × 页面最长边(pt)`；
- **钳制在 `[1000, 2500]` px**：`MIN_RENDER_LONGEST_SIDE = 1000`、`MAX_RENDER_LONGEST_SIDE = 2500`。
- 超大的扫描页（如 3000×4000pt）若按原样栅格化会 >200 Mpx，又慢又超 OCR 引擎图像解码上限，所以必须钳制。2500px ≈ A4 的 214 DPI。

### 6.2 OCR 引擎优先级与降级

**源码**：`extractor/ocr.rs::preferred_engine` / `platform_default_engine`

```
按平台原生优先（Windows OCR / macOS Apple Vision，进程内、无黑窗）
  → 内置 PaddleOCR（PP-OCRv5）
  → Tesseract（需自装 CLI）
```

配置的引擎在本机不可用时（如 Windows 上残留的 macOS 默认值、PaddleOCR 模型未下载）自动回退到可用引擎。图像进入 PaddleOCR 前还会经 `preprocess::preprocess_for_ocr`（升采样 + 保梯度增强，**不做二值化**，因为 DBNet 需要梯度）。

### 6.3 全局并发闸门

**源码**：`extractor/ocr.rs::OcrGate`

`OCR_GATE` 限制**同时进行的 OCR 推理数 = min(核数, 8)**，防止「batch_index 并行 × PDF 页并行」的嵌套 fan-out 把 CPU 拖爆（每页退化到 2.5s+）。硬件自适应，无用户设置。

### 6.4 看门狗（stall watchdog）

**源码**：`pdf.rs::global_ocr_stall_timeout`，默认 `DEFAULT_OCR_STALL_TIMEOUT = 120s`

- 语义是「**零进度**持续多久算卡死」，**不是**整篇墙钟预算——慢机器只要持续有进展就放行，只有真卡住才杀；
- 优先级：环境变量 `LINK_SEARCHER_OCR_STALL_SECS` > 设置项 `ocr_stall_timeout_secs` > 默认 120s；设 `0` 关闭看门狗。

### 6.5 逐页拼接：`merge_page_texts`

**源码**：`pdf/ocr.rs`

- 页数取 `max(文本层页数, 最大 OCR 页号 + 1)`；
- 每页**优先用 OCR 结果**，没有 OCR 则用原文本层，最后 `\n` 拼接；
- **关键回归**：调用「整篇强制 OCR」时会传**空**文本层，旧实现只遍历文本层会把所有 OCR 结果丢掉（实测某 145 页卷宗丢了 142 页），现在必须保留空文本层下的 OCR 结果。

拼接后的文本回到**主图 17** 的「清洗 + 质量评分」步骤，后续与其它格式一致。

---

## 7. 关键参数与阈值速查

| 参数 | 值 | 位置 |
|------|----|------|
| 整篇 vs 分页页数阈值 `LARGE_SCAN_PAGE_THRESHOLD` | 20 | `pdf.rs` |
| 整页图覆盖率 `FULL_PAGE_IMAGE_COVERAGE` | 0.8 | `pdf/scan.rs` |
| 页内图最小有效面积 `MIN_PAGE_IMAGE_AREA` | 100 000 px² | `pdf/ocr.rs` |
| 渲染最长边钳制 | 1000 ~ 2500 px | `pdf/ocr.rs` |
| 单页 pdftoppm 超时 | 30s | `pdf/ocr.rs` |
| 整篇 pdftoppm / pdftotext 超时 | 120s | `pdf/ocr.rs` / `pdf.rs` |
| OCR 卡死看门狗 `DEFAULT_OCR_STALL_TIMEOUT` | 120s（`0` 关闭） | `pdf.rs` |
| OCR 并行闸门 | min(核数, 8) | `extractor/ocr.rs` |
| 文本层过稀阈值 | 50 非空白字符/页 | `pdf/quality.rs` |
| 乱码阈值 | 可疑字符 >30% 或非空白 <5% | `pdf/quality.rs` |
| 水印阈值 | 相邻页归一化前缀相同 >80% | `pdf/quality.rs` |
| 重复阈值 | 重复行占比 >60% | `pdf/quality.rs` |
| 不可信密度 | >20000 非空白字符/页 | `pdf/quality.rs` |
| 渲染 DPI 设置 `ocr_pdf_dpi` | 默认 300，钳制 [100, 600] | `pdf.rs` |
| 前 N 页全空则中止 | 5 页 | `pdf/ocr.rs` |

---

## 8. 常见排查线索（日志关键字）

| 现象 | 日志关键字 / 线索 |
|------|------------------|
| PDF 解析失败转兜底 | `[PDF] ... lopdf failed to parse ... trying pdftotext/anydoc fallback` |
| pdftotext 恢复成功 | `[PDF] ...: pdftotext recovered ... chars` |
| 判为扫描件 | `[PDF] ...: image-based scan — OCR bypassed text layer` |
| 无文本层强制 OCR | `[PDF] ...: no text layer at all across N pages — forcing OCR` |
| 大文档分页 OCR | `running per-page OCR loop for N pages` |
| 旋转页跳过 pdfimages | `pages carry /Rotate — skipping pdfimages (it ignores page rotation)` |
| pdfimages 结果判废 | `pdfimages OCR for ... looks garbled ... retrying via pdftoppm` |
| OCR 卡死 | `per-page OCR stalled — page N took ...` |
| 逐页拼接结果 | `per-page OCR completed: attempted a/b pages, got text for c pages` |

> 佐证运行日志：`logs/scan-{时间戳}.log`（每次扫描独立日志）。

---

## 9. 一句话总结

**PDF 提取 = 能抽就抽、抽不到判真假、真假不分明就 OCR、OCR 也要质检和兜底。**

①→②→③ 尽量拿到干净文本层；④/⑤ 决定「文本层可不可信、是不是扫描件/旋转件」；⑥/⑦ 才真正投入 OCR（整篇 or 分页、pdfimages or pdftoppm、多引擎降级），最后逐页拼接回到主流程。

---

🔗 相关：[主图 17 文件生命周期](17-file-lifecycle.html) · [文本提取管线（04）](04-extraction-pipeline.html) · [设计手册 ARCHITECTURE.md 模块二](../ARCHITECTURE.md)
