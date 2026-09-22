# 文本提取层特殊分支清单

> 本文档是维护参考，从 `src-tauri/src/extractor/` 源码和 `CHANGELOG.md` 中提取。
> 所有函数名、常量、阈值均按源码核实。分支计数为近似值，以代码为准。
> 最后核实日期：2026-09-21，对应 `extractor/` 模块树。

---

## 1. 概述

文本提取层是 Link-Searcher 最复杂的子系统。表面上有 9 种文件类型、约 95 条特殊分支，
但核心矛盾只有一个：**PDF 是三层不确定性的叠加**。

其他文件类型的提取要么直读（纯文本）、要么交给单一库（Office）、要么走 OCR（图片）。
只有 PDF 同时拥有文本层、图像层和外部工具链，三个层都可能损坏或缺失，彼此之间还要做取舍。
理解了 PDF 的决策链，其他类型的分支就只是单一维度的防护。

本文档按两个维度组织：维度一按文件类型（第 2 节），维度二按病理机制（第 3 节）。
第 4 节给出 PDF 的完整决策流程图，第 5 节汇总所有硬编码阈值。
第 6 节用 CHANGELOG 统计佐证历史问题分布，第 7 节给出三个真实案例，第 8 节指出已知缺口。

---

## 2. 维度一：按文件类型（9 大类）

顶层路由函数 `classify_ext`（`extractor/mod.rs`）按扩展名返回 7 种类型：
`image` / `pdf` / `office` / `text` / `archive` / `audio` / `unknown`。

实际提取入口 `extract_text_with_meta`（`extractor/mod.rs`）的路由表：

| # | 类型 | 扩展名 | 提取路径 | 源文件 |
|---|------|--------|----------|--------|
| 1 | 纯文本 | `txt md csv json xml yaml yml toml ini cfg log py rs ts js html css sql sh bat ps1 env conf properties` | `TextExtractor` + 编码检测 | `text.rs` |
| 2 | PDF | `pdf` | `PdfExtractor::extract_with_lang` 多阶段 | `pdf.rs` |
| 3 | Office legacy | `doc` | `rwml` 纯 Rust 解析 | `office/mod.rs` |
| 4 | Office 表格 | `xls xlsx xlsm xlsb` | `calamine`（每格式独立 reader） | `office/mod.rs` |
| 5 | Office 现代 | `docx docm ppt pptx pptm ppsm ppsx pps pot odt ods odp rtf epub` | `anydoc::to_markdown` | `office/mod.rs` |
| 6 | 图片 | `png jpg jpeg gif bmp webp tiff tif` | OCR 引擎调度 | `mod.rs` -> `ocr.rs` |
| 7 | 压缩包 | `zip tar tgz tbz2 txz gz bz2 xz` | 逐条目重新提取 | `archive.rs` |
| 8 | 音频 | `mp3 wav m4a aac flac ogg opus wma` | FunASR + VAD + 说话人分离 | `audio.rs` |
| 9 | 未知回退 | 任意 | 读前 10MB 作 UTF-8，二进制则拒绝 | `mod.rs` |

注意：`image.rs` 仅含测试代码，生产环境的图片 OCR 路由在 `mod.rs` 第 94-110 行直接调用
`ocr::ocr_image_with_stats`。

---

## 3. 维度二：按病理/特殊条件

### 3.1 PDF（约 35 条）

PDF 分支按问题来源分 7 组。

**加载/panic（5 条）**

| 病理 | 处理 | 函数 |
|------|------|------|
| lopdf load 返回 Err | pdftotext -> anydoc -> 图片 OCR | `extract_with_lang` |
| lopdf load panic | `catch_unwind` 捕获 -> pdftotext -> 图片 OCR | `extract_with_lang` |
| 空页面集（pages.is_empty） | 返回空字符串 | `extract_with_lang` |
| 单页 `extract_text` 返回 Err | 该页存空字符串，继续 | `extract_with_lang` |
| 单页 `extract_text` panic | `catch_unwind` 捕获，该页存空字符串 | `extract_with_lang` |

**字体/编码（2 条）**

| 病理 | 处理 | 函数 |
|------|------|------|
| 字体无 Name 值的 `/Encoding` 但有 `/ToUnicode` | 跳过 lopdf，走 pdftotext（需通过 garbled/implausible/sparse 检查） | `has_unparseable_font_encoding` |
| Quartz/CFF PDF（lopdf + pdftotext 均失败） | `anydoc::to_markdown`（需 >100 字符） | `extract_with_lang` |

**扫描件/图像（4 条）**

| 病理 | 处理 | 函数 |
|------|------|------|
| `is_image_based_scan`（>=50% 页有满版图像，覆盖率 >=0.8 两维度） | 直接走 OCR，跳过文本层 | `is_image_based_scan` |
| `pdf_inspector` 判定 Scanned/ImageBased 或 all_need_ocr | 直接走 OCR | `classify_pdf_mem` |
| pdfimages 最大图 <100K px^2 | 判定"非扫描页"，拒绝 OCR | `extract_and_ocr_page_via_pdfimages` |
| pdfimages 不足 50% 页有图 | 判定"非扫描 PDF"，返回错误 | `ocr_pdf_via_pdfimages` |

**文本层质量（7 条）**

| 病理 | 判定条件 | 函数 |
|------|----------|------|
| 乱码 | >30% 可疑字符 或 <5% 非空白 | `is_garbled_text` |
| 水印 | >80% 相邻页归一化前缀相同 | `is_watermark_text` |
| 重复 | >60% 非空行重复 | `is_repetitive` |
| 稀疏 | <50 非空白字符/页 | `is_sparse_text_layer` |
| 不合理 | >20_000 非空白字符/页 或 LowPrintable flag | `is_implausible_text_layer` |
| 综合判定 | `doc_clean = len>100 && !garbled && !wm && !rep && !sparse && !implausible` | `extract_with_lang` |
| 文本层不可信时恢复 | `!doc_clean` -> `try_pdftotext_extract` + `prefer_recovered_text` | `extract_with_lang` |

**混合文档（3 条）**

| 病理 | 处理 | 函数 |
|------|------|------|
| 部分页需 OCR | `page_needs_ocr` 逐页判定 -> `ocr_single_pdf_page` + `merge_page_texts` 拼接 | `extract_with_lang` |
| 0 页需 OCR | 返回 lopdf 文本层 | `extract_with_lang` |
| 逐页 OCR 部分失败 | 整文档 OCR 回退 | `extract_with_lang` |

**大扫描件（3 条）**

| 病理 | 处理 | 函数 |
|------|------|------|
| 页数 >20 或整文档 OCR 失败 | `should_ocr_pages_individually` 返回 true | `should_ocr_pages_individually` |
| 逐页 OCR 循环 | 带时间预算（600s），超时提前停止 | `run_pdf_ocr_pipeline` |
| 预算耗尽 | 记录已处理页数，合并已有结果 | `run_pdf_ocr_pipeline` |

**OCR 链/工具链（5 条）**

| 病理 | 处理 | 函数 |
|------|------|------|
| pdfimages 优先（水印污染少） | 先 `ocr_pdf_via_pdfimages`，后 `ocr_pdf_via_pdftoppm` | `try_ocr_fallback` |
| pdfimages OCR 结果 >100 字符才接受 | 否则尝试 pdftoppm | `try_ocr_fallback` |
| poppler 缺失 | `is_image_based_scan` 返回 false，图片 OCR 静默禁用 | `find_poppler_binary` |
| 页数获取 | pdfinfo 优先（容忍 lopdf 拒绝的损坏流），lopdf 兜底 | `get_pdf_page_count` |
| pdftotext 结果过滤 | <100 字符 / 水印 / 重复 -> 返回 None | `try_pdftotext_extract` |

**DPI（1 条）**

| 病理 | 处理 | 函数 |
|------|------|------|
| DPI 配置缺失/非法 | 默认 300，clamp [100, 600] | `normalize_pdf_dpi` |

另有辅助函数 `page_media_size`（跟随 Parent 获取继承的 MediaBox）、
`full_page_image_pages`（解析 pdfimages -list 输出）、
`normalize_for_watermark`（剥离 >=30 字符的十六进制串、日期、URL），
不计入主分支但影响判定结果。

### 3.2 纯文本（8 条）

| 病理 | 处理 | 函数/常量 |
|------|------|-----------|
| 大文件 | `take(10 * 1024 * 1024)` 截断 10MB | `TextExtractor::extract` |
| 空文件 | 返回 `Ok(String::new())` | `TextExtractor::extract` |
| UTF-16LE BOM | `encoding_rs::UTF_16LE.decode_without_bom_handling` | `TextExtractor::extract` |
| UTF-16BE BOM | `encoding_rs::UTF_16BE.decode_without_bom_handling` | `TextExtractor::extract` |
| UTF-8 BOM | 跳过前 3 字节 | `TextExtractor::extract` |
| 二进制检测 | 前 8KB 含 NUL -> 返回空字符串 | `BINARY_CHECK_SIZE = 8_192` |
| GBK/GB18030 回退 | UTF-8 失败且无 BOM 时尝试 GBK | `TextExtractor::extract` |
| lossy vs GBK 选优 | 比较两者 FFFD 数量，取较少者 | `TextExtractor::extract` |

### 3.3 Office（6 条）

| 病理 | 处理 | 函数 |
|------|------|------|
| .doc 空或损坏 | 返回错误"Word 文档无文本内容（可能损坏或加密）" | `doc_via_rwml` |
| 表格空或损坏 | 返回错误"电子表格无文本内容（可能损坏或加密）" | `xls_via_calamine` |
| anydoc 空结果 | 返回错误"文档无文本内容（可能损坏或加密）" | `office_via_anydoc` |
| anydoc 加密 | 捕获 `ConvertError::Encrypted`，返回"此文件已加密，无法读取内容" | `office_via_anydoc` |
| 表格单元格类型分派 | `String` / `Float` / `Int` / `Bool` / `DateTime` / `DateTimeIso` / `DurationIso` | `append_sheet_text` |
| LibreOffice 回退已移除 | 模块文档记录：soffice 只在损坏文件上触发且同样失败，已删除 | `office/mod.rs` 模块注释 |

### 3.4 压缩包（9 条）

| 病理 | 处理 | 函数/常量 |
|------|------|-----------|
| 总大小上限 | `MAX_TOTAL_SIZE = 100MB`，超限后跳过 | `ArchiveExtractor` |
| 条目数上限 | `MAX_FILES = 1000`，超限后跳过 | `ArchiveExtractor` |
| 单条目上限 | `MAX_SINGLE_FILE = 50MB`，超限跳过该条目 | `ArchiveExtractor` |
| 解压超限检测 | `read_capped` 读 cap+1 字节，`over=true` 则跳过 | `read_capped` |
| zip-slip 防护 | 绝对路径或含 `..` 段 -> 拒绝 | `is_safe_archive_name` |
| 条目大小跳过 | `entry_size > MAX_SINGLE_FILE` -> 跳过 | `extract_zip` / `process_tar_entries` |
| 嵌套条目重新提取 | 写临时文件 -> `extract_text` 递归 | `append_entry` |
| 错误分类 | 加密 / 损坏 / 提取失败 三类 | `classify_extract_error` |
| .tar.gz 显示名修复 | 去掉 `.tar` 后缀取真实 stem | `extract_single_compressed` |

### 3.5 音频（9 条）

| 病理 | 处理 | 函数/常量 |
|------|------|-----------|
| ASR 模型未下载 | 返回占位文本"ASR 模型未下载" | `extract_audio` |
| ffmpeg 解码超时 | 600s 超时杀进程 | `extract_audio` |
| ffmpeg 解码失败 | 返回错误"ffmpeg decode failed" | `extract_audio` |
| recognizer 初始化失败 | 返回占位文本"ASR 初始化失败" | `extract_audio` |
| 空转写结果 | 返回占位文本"FunASR 推理完成，无识别结果" | `extract_audio` |
| Silero VAD 分段 | `vad_segments` 可用时按语音活动分段 | `vad_segments` |
| 固定 28s 硬切 | VAD 不可用时 28s 分段 + 1s 重叠 | `recognize_segments` |
| 说话人分离 | `SPEAKER_DIARIZER` 可用时标注说话人 | `extract_audio` |
| 分离失败回退 | `d.process(seg)` 返回 None -> 无说话人标注直接转写 | `extract_audio` |

> 注意：`audio.rs` 第 287 行设置 600s 超时，但第 294 行的 bail 消息写的是 "timed out after 300s"，
> 数值不一致。这是代码中的已知笔误，不影响行为（实际超时是 600s）。

### 3.6 OCR 引擎（14 条）

| 病理 | 处理 | 函数/常量 |
|------|------|-----------|
| 引擎回退链 | `preferred_engine` -> `platform_default_engine` | `ocr.rs` |
| 全局并发闸门 | `OcrGate` cap = min(cores, 8) | `ocr.rs` |
| Tesseract 语言回退 | 请求语言不可用 -> "eng" | `ocr_image_tesseract` |
| Tesseract 超时 | 120s 轮询超时 | `ocr_image_tesseract` |
| Tesseract 二值化预处理 | 灰度 -> 2x 放大 -> 高斯去噪 -> 自适应二值化 -> 形态学开运算 | `preprocess_image` |
| PaddleOCR 锁超时 | 120s 超时返回错误 | `with_engine_timed` |
| PaddleOCR 置信度过滤 | `MIN_CONFIDENCE = 0.5` 以下丢弃 | `recognize_from_path_enriched` |
| PaddleOCR 小图放大 | 最长边 <1000px -> 放大到 1000px | `preprocess.rs` `MIN_LONGEST_SIDE` |
| 低对比度均衡 | p95-p5 <100 -> `equalize_histogram` | `preprocess.rs` `LOW_CONTRAST_THRESHOLD` |
| 倾斜矫正 | 投影方差搜索 +-10 度，2-15 度之间矫正 | `preprocess.rs` `detect_deskew_angle` |
| Windows OCR 超限缩放 | 任一边 >4000px -> 缩放 | `windows_ocr.rs` `WIN_OCR_MAX_DIM` |
| Windows OCR 语言包回退 | 尝试 BCP-47 列表 -> `TryCreateFromUserProfileLanguages` | `create_engine` |
| Apple Vision autoreleasepool | 每次调用包裹 autoreleasepool 防泄漏 | `apple_vision.rs` |
| Apple Vision 语言回退 | 请求语言不在映射表 -> "en-US" | `apple_vision.rs` |

> Tesseract 预处理（`ocr.rs::preprocess_image`）做二值化；
> PaddleOCR 预处理（`preprocess.rs`）不做二值化，保留梯度信息供 DBNet 使用。
> 两套预处理管线的设计目标不同，不可混用。

### 3.7 质量评分（5 个信号 flag + 1 复合阈值）

`quality.rs` 中 `QualityFlag` 枚举有 7 个变体，其中 5 个是提取质量信号，
另 2 个（`Exhausted`、`MaxReextract`）是重提取控制标志，不参与评分计算。

| flag | 触发条件 | 常量 |
|------|----------|------|
| `LowPrintable` | `printable_ratio < 0.7` | `compute_quality` |
| `HighFffd` | `fffd_ratio > 0.15` | `compute_quality` |
| `LowConfidence` | `ocr_used && confidence < 0.6` | `compute_quality` |
| `LowDensity` | `density_norm < 0.25` | `compute_quality` |
| `LowLexicon` | `lexicon_hit_rate < 0.4` | `compute_quality` |

复合评分公式（`compute_quality`）：

```
score = 0.20 * printable_ratio
      + 0.15 * (1 - fffd_ratio)
      + 0.25 * confidence_component
      + 0.15 * density_norm
      + 0.25 * lexicon_hit_rate
```

权重之和 = 1.0，score 取值 [0, 1]。`confidence_component` 在非 OCR 场景恒为 1.0。
复合阈值 0.75 在 UI 和 DB 层使用（见 README "低于 0.75 标记低质量"），
`quality.rs` 本身未定义该常量。

### 3.8 调度清洗（3 条）

| 病理 | 处理 | 函数 |
|------|------|------|
| 未知扩展名回退 | 读前 10MB，UTF-8 解码，空白或二进制则拒绝 | `extract_text_with_meta` 的 `_` 分支 |
| sanitize_text NUL | 含 NUL -> 返回空字符串 | `sanitize_text` |
| sanitize_text 高 FFFD | FFFD 占比 >15% -> 替换为空格 | `sanitize_text` |

### 3.9 扫描器 helpers（6 条）

| 病理 | 处理 | 函数/常量 |
|------|------|-----------|
| 网络 FS metadata 挂起 | 辅助线程 + 超时，超时则跳过条目 | `metadata_timeout` |
| 排除特定文件名 | `.DS_Store` `Thumbs.db` `.git` `.svn` `__pycache__` | `EXCLUDED_NAMES` |
| 排除前缀 | `#` `$` `.` `~` | `EXCLUDED_PREFIXES` |
| 排除后缀 | `.tmp` `.temp` `.bak` `.swp` `.swo` `~` | `EXCLUDED_SUFFIXES` |
| Windows verbatim 路径 | `\\?\` 和 `\\?\UNC\` 前缀剥离，反斜杠转正斜杠 | `normalize_os_path` |
| 陈旧重索引 | indexed=1 但 md5 缺失 -> 触发重索引 | `needs_reindex` |

---

## 4. PDF 决策链

`PdfExtractor::extract_with_lang`（`pdf.rs`）的完整流程，按执行顺序：

```
                    extract_with_lang(path, lang, engine)
                              |
                   v----------+----------v
                   | preferred_engine     |
                   | lopdf::Document::load|
                   | (catch_unwind)       |
                   +----------+-----------+
                              |
                    +---------+---------+
                    | load Ok           | load Err / panic
                    v                   v
          +-------------------+    pdftotext -> anydoc -> 图片 OCR
          | pages = get_pages |    (catch_unwind 两条路径分别处理)
          +---------+---------+
                    |
          +---------+---------+
          | (a) is_image_     |  true
          | based_scan?       |-----> run_pdf_ocr_pipeline -> Ok(ocr_text)
          +---------+---------+
                    | false
          +---------+---------+
          | (b) has_unparseable|  true 且 pdftotext 通过
          | _font_encoding?   |-----> Ok(pdftotext text)
          +---------+---------+
                    | false / 未通过
          +---------+---------+
          | 逐页 extract_text  |
          | (catch_unwind/页)  |
          +---------+---------+
                    |
          +---------+---------+
          | merged = join("\n")|
          +---------+---------+
                    |
          +---------+---------+
          | 信号采集:           |
          | is_sparse           |
          | is_implausible     |
          | pdf_inspector::    |
          |   classify_pdf_mem  |
          |   (仅 merged.len()>100)
          |   -> Scanned/      |
          |   ImageBased/      |
          |   all_need_ocr -> OCR
          | is_wm, is_garbled, |
          | is_rep             |
          +---------+---------+
                    |
          +---------+---------+
          | doc_clean =        |
          |   len>100           |
          |   && !garbled       |
          |   && !wm            |
          |   && !rep           |
          |   && !sparse        |
          |   && !implausible   |
          +---------+---------+
                    |
           +--------+--------+
           | !doc_clean      | doc_clean
           v                 v
  try_pdftotext_extract    逐页精修:
  + prefer_recovered_text  page_needs_ocr
  -> Ok(recovered) or      |
  fall through             +--+--+
                           |0 |   | some | all/partial-fail
                           v  v   v
                        Ok(merged)  ocr_single_pdf_page
                                   + merge_page_texts
                                   -> Ok(merged_text) or fall through
                    |
          +---------+---------+
          | run_pdf_ocr_pipeline|
          | (最终兜底)          |
          +---------+---------+
                    |
               Ok(merged) 兜底
```

关键决策点：

- **步骤 0**：`preferred_engine` 解析引擎，`catch_unwind` 包裹 lopdf load。
  load Err 和 panic 走不同回退路径（panic 路径不尝试 anydoc）。
- **步骤 1a**：`is_image_based_scan` 在 lopdf 成功后、逐页提取前执行。
  判定条件：`pdfimages -list` 输出中，满版图像页 >= 总页数的一半
 （`full.len() * 2 >= page_ids.len()`）。满版图像的判定见 `full_page_image_pages`，
  要求图像在宽高两个维度均覆盖页面的 >=80%（`FULL_PAGE_IMAGE_COVERAGE = 0.8`）。
- **步骤 1b**：`has_unparseable_font_encoding` 检查字体是否有 Name 值的 `/Encoding`。
  无则 lopdf 会静默丢字，此时用 pdftotext 恢复（需通过 garbled/implausible/sparse 三项检查）。
- **步骤 3**：`pdf_inspector::classify_pdf_mem` 仅在 `merged.len() > 100` 时调用，
  避免对空文本层做无意义分类。`all_need_ocr` 判定：`pages_needing_ocr` 数量过半或 is_sparse。
- **步骤 4**：`doc_clean` 是文档级"文本层可信"的总开关。任一信号为 true 即推翻。
- **步骤 5**：`doc_clean=true` 时才做逐页精修。`page_needs_ocr` 用 pdf_inspector 的逐页信息
  （有则用，无则用 `is_sparse_text_layer(text, 1)` 兜底）。0 页需 OCR 则直接返回 merged。
- **步骤 6**：所有路径最终汇聚到 `run_pdf_ocr_pipeline` 或返回 merged。

---

## 5. 硬编码阈值/常量总表

以下常量均从源码核实，按文件分组。

### pdf.rs

| 常量/阈值 | 值 | 函数 | 用途 |
|-----------|-----|------|------|
| `LARGE_SCAN_PAGE_THRESHOLD` | 20 | `should_ocr_pages_individually` | 超过则逐页 OCR |
| `LARGE_SCAN_OCR_BUDGET` | 600s | `run_pdf_ocr_pipeline` | 逐页 OCR 时间预算 |
| `FULL_PAGE_IMAGE_COVERAGE` | 0.8 | `full_page_image_pages` | 满版图像覆盖率 |
| `MIN_PAGE_IMAGE_AREA` | 100_000 px^2 | `extract_and_ocr_page_via_pdfimages` | 最小扫描页图像面积 |
| 稀疏阈值 | <50 非空白字符/页 | `is_sparse_text_layer` | |
| 乱码阈值 | >30% 可疑字符 或 <5% 非空白 | `is_garbled_text` | |
| 水印阈值 | >80% 相邻页前缀匹配 | `is_watermark_text` | |
| 重复阈值 | >60% 非空行重复 | `is_repetitive` | |
| 不合理阈值 | >20_000 非空白字符/页 | `is_implausible_text_layer` | |
| pdftotext 接受下限 | 100 字符 | `try_pdftotext_extract` | |
| anydoc 接受下限 | 100 字符 | `extract_with_lang` | |
| pdfimages-OCR 接受下限 | 100 字符 | `try_ocr_fallback` | |
| DPI 默认/范围 | 300, [100, 600] | `normalize_pdf_dpi` | |
| pdfinfo 超时 | 60s | `get_pdf_page_count` | |
| pdfimages -list 超时 | 60s | `is_image_based_scan` | |
| pdftotext 超时 | 120s | `try_pdftotext_extract` | |
| pdftoppm 整文档渲染超时 | 120s | `ocr_pdf_via_pdftoppm` | |
| 逐页 pdftoppm 超时 | 30s | `ocr_single_pdf_page` | |
| 逐页 pdfimages 超时 | 30s | `extract_and_ocr_page_via_pdfimages` | |

### text.rs

| 常量 | 值 | 用途 |
|------|-----|------|
| `BINARY_CHECK_SIZE` | 8_192 | 二进制检测窗口 |
| 读上限 | 10MB | `take(10 * 1024 * 1024)` |

### archive.rs

| 常量 | 值 | 用途 |
|------|-----|------|
| `MAX_TOTAL_SIZE` | 100 * 1024 * 1024 (100MB) | 解压总大小上限 |
| `MAX_FILES` | 1000 | 条目数上限 |
| `MAX_SINGLE_FILE` | 50 * 1024 * 1024 (50MB) | 单条目上限 |

### audio.rs

| 常量/阈值 | 值 | 用途 |
|-----------|-----|------|
| ffmpeg 解码超时 | 600s | `extract_audio` 中 deadline |
| `CHUNK` | 28 * 16000 (28s) | `recognize_segments` 硬切段 |
| `OVERLAP` | 16000 (1s) | `recognize_segments` 重叠 |
| VAD `max_speech_duration` | 30.0s | `vad_segments` |

### paddleocr.rs

| 常量 | 值 | 用途 |
|------|-----|------|
| `MIN_CONFIDENCE` | 0.5 | 置信度过滤 |
| 引擎锁超时 | 120s | `with_engine_timed` |

### ocr.rs

| 常量/阈值 | 值 | 用途 |
|-----------|-----|------|
| `OcrGate` cap | min(cores, 8) | 全局 OCR 并发上限 |
| Tesseract 超时 | 120s | `ocr_image_tesseract` |

### windows_ocr.rs

| 常量 | 值 | 用途 |
|------|-----|------|
| `WIN_OCR_MAX_DIM` | 4000 | 超限缩放阈值 |

### preprocess.rs

| 常量 | 值 | 用途 |
|------|-----|------|
| `MIN_LONGEST_SIDE` | 1000 | 小图放大阈值 |
| `TARGET_LONGEST_SIDE` | 1000 | 放大目标 |
| `LOW_CONTRAST_THRESHOLD` | 100 | 低对比度判定 |
| `DESKEW_MIN_ANGLE_DEG` | 2.0 | 最小矫正角度 |
| `DESKEW_MAX_ANGLE_DEG` | 15.0 | 最大矫正角度 |
| `DESKEW_DETECT_MAX_SIDE` | 400 | 检测缩放边 |

### quality.rs

| 阈值 | 值 | flag |
|------|-----|------|
| printable_ratio | <0.7 | `LowPrintable` |
| fffd_ratio | >0.15 | `HighFffd` |
| OCR confidence | <0.6 | `LowConfidence` |
| density_norm | <0.25 | `LowDensity` |
| lexicon_hit_rate | <0.4 | `LowLexicon` |
| 复合评分（UI/DB 层） | <0.75 | 标记低质量 |

### mod.rs (sanitize_text)

| 阈值 | 值 | 用途 |
|------|-----|------|
| FFFD 替换比 | >15% | NUL -> 空，FFFD >15% -> 替换为空格 |

---

## 6. 历史佐证（CHANGELOG 统计）

`CHANGELOG.md` 共 3760 行，280 条 `##` 级条目，其中含"根因"的条目 93 条。
以下为关键词命中行数（`grep -c` 统计，2026-09-21 核实）：

| 关键词 | 命中行数 | 关键词 | 命中行数 |
|--------|----------|--------|----------|
| OCR | 163 | 扫描件 | 17 |
| 重复 | 52 | pdftotext | 17 |
| 超时 | 45 | poppler | 15 |
| 编码 | 34 | 乱码 | 15 |
| ASR | 34 | pdfimages | 12 |
| 音频 | 27 | 稀疏 | 9 |
| pdftoppm | 23 | 压缩包 | 8 |
| 图片 | 23 | 大文件 | 8 |
| 水印 | 19 | 字体 | 7 |
| 损坏 | 19 | 加密 | 6 |
| | | Quartz | 3 |
| | | 多栏 | 2 |

OCR 以 163 次命中居首，远超第二名"重复"（52）。PDF 相关关键词
（pdftoppm/pdftotext/poppler/pdfimages/扫描件/Quartz/字体/水印/乱码/稀疏）
合计超过 200 次命中，印证 PDF 是历史问题最集中的子系统。

---

## 7. 真实案例

以下三个文件来自项目实际案件库，覆盖 PDF 提取的三种典型失败模式。

### 案例 1：一审判决书.pdf（macOS Quartz 扫描件 + 合成文字层）

- **来源**：macOS Quartz 生成的扫描 PDF，18 页
- **问题**：字体声明无 Name 值的 `/Encoding`，fallback 字形
  （人/行/自/一/用/月/日/身/生...）被拆分到独立 text run 中
- **lopdf 行为**：丢字，输出看似干净但缺字
- **pdftotext 行为**：保留字符但破坏阅读顺序（散落字符堆在页尾）
- **pdf-inspector 判定**：`TextBased`（conf=1.0），因为检测器只看 Tj/TJ 算子，
  不看图像维度 -> 应用信任了损坏的文本层
- **修复**：`is_image_based_scan` 判定 true -> 走 Apple Vision OCR
- **结果**：正确有序文本，10,293 字符，耗时 8.24s

### 案例 2：禾苗公司 20151110.pdf（iText 全页扫描 + 防伪水印文字层）

- **来源**：`案件/CH 常宏案/05 工商内档/禾苗公司/20151110.pdf`，iText 生成，34 页
- **问题**：每页有 CCITT 满版扫描图像，文本层仅含防伪/追踪水印垃圾
  （3:4 / 4:3 / 202 / 4-0 / 7-0，共 9222 字符）
- **旧路径**：`is_repetitive=true` -> `doc_clean=false` -> 返回 pdftotext 的水印垃圾
  （9222 > 8120 字符，超过了旧阈值）
- **修复**：`is_image_based_scan=true`（20/34 页有满版图像）-> OCR
- **结果**：正确内容（准予设立/开业登记通知书... 上海禾苗股权投资基金管理有限公司...，
  15,987 字符）

### 案例 3：1-民事裁定书简转普.pdf（WPS 嵌入式水印，文本层完好）

- **来源**：`案件/WC 万城/诉讼案件/股东资格/阅卷/41833/本院法律文书正本/1-民事裁定书简转普.pdf`
  ，WPS 生成，4 页，TEXT PDF（非扫描件）
- **问题**：文本层有完整正确内容，但 WPS 姓名/编号水印（陈骥 + X34**123）
  插在句子中间
- **`is_watermark_text` 判定**：false。检测器只比较每页前 300 字符的归一化前缀，
  而此水印是散布式（不是前缀）
- **实测权衡**：文本层 1237 字符，字节正确；Vision OCR 1037 字符，阅读顺序正确但有 OCR 错误
  （小城->小坡/永誠，郑坚敏：勇），且水印被视觉渲染所以 OCR 也读到了（陈骥321***）
- **结论**：对这类文件，OCR 更差，应保留文本层

---

## 8. 已知缺口与收敛方向

### 8.1 `is_wm` 在 `doc_clean` 中的过度否决

`is_watermark_text` 返回的 `is_wm` flag 在 `extract_with_lang` 中只有一个功能性用途：
参与 `doc_clean` 的 `!is_wm` 条件（另有一行日志输出）。效果是：一旦检测到水印，
`doc_clean` 变 false，跳过整个逐页精修分支，将文档推入 pdftotext 恢复或整文档 OCR。

这个否决过于宽泛。水印是可去除的噪声，不是文本层不可用的证据。
历史上的理由（iText PDF 中 lopdf 只提取出水印）已被 `!is_sparse` 和 `!is_rep` 覆盖。

`is_watermark_text` 在 `try_pdftotext_extract` 中也有使用，用于拒绝含水印的 pdftotext
结果。这个用途合理，应保留。

**注意事项**：尚未观察到 `wm=true` 且正文健康的真实文件。提出的修改（从 `doc_clean`
中去掉 `!is_wm`）**未经验证**，不应在没有复现样本的情况下进行。建议先扫描现有日志中
`wm=true garbled=false rep=false sparse=false implausible=false` 的行，确认是否存在这类文件，
再决定是否修改。

### 8.2 四大家族归纳

约 95 条特殊分支可归约为四类：

1. **文本层不可信**（字体/编码/水印/乱码/稀疏/重复/密度）。
   PDF 独有，因为只有 PDF 有可编程的文本层。
2. **图像层替代**（扫描检测/逐页混合/大扫描件预算）。
   PDF 独有，因为只有 PDF 同时有文本层和图像层。
3. **工具链缺失/超时**（poppler、子进程超时、引擎回退链）。
   PDF 依赖最重，音频次之（ffmpeg），图片再次（OCR 引擎）。
4. **资源上限**（10/50/100MB、页数预算、并发闸门）。
   普适，但 PDF 的 600s 逐页预算和 1000 文件压缩包上限是最容易触发的。

PDF 独占第 1-3 类，因为它同时组合了文本层 + 图像层 + 外部工具链。
结论：不应继续增加 `is_xxx` 谓词，而应收敛到清晰的优先级链 + 可测量的门控，
正如扫描检测修复（`is_image_based_scan`）所做的那样：用一个可测量的信号
（满版图像页占比 >=50%）替代多个启发式猜测。
