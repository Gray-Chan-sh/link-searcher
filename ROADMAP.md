# Link-Searcher 路线图

> 最后更新：2026-11-01

---

## ✅ 本轮已完成

- [x] **索引优化 Wave 1**：SQLite `synchronous=NORMAL` + `cache_size=-8000` + `temp_store=MEMORY` + PRAGMA optimize + pool 32→12 + Tantivy writer 150MB/500 chunk + PaddleOCR pool auto-size + 移除死代码 `update_hotwords`
- [x] **索引优化 Wave 2**：`replace_chunks` 事务化 + Phase-2 `update_indexed_batch` 批量事务 + `run_backfill_chunks` 改用 `char_count` 列 + 7×图片 COUNT 合并为单查询 + pending-files 部分索引
- [x] **索引优化 Wave 3**：`pkill -f` → `child.kill()` 正确性修复（不再误杀系统中所有 pdftoppm/pdfimages 进程）
- [x] **索引优化 Wave 4**：`build_schema()` 改用 `OnceLock` 缓存，消除每次 add/delete/search 调用的冗余构建
- [x] **索引优化 Wave 5**：扫描进度实时显示（从事件读取 processed/total）、ResultList scroll rAF 节流、suggest 请求 ID 防竞态、retryFailed await 修复

- [x] **AnyDoc 集成**：Office 格式和文字 PDF 统一用 anydoc，LO 降为备用
- [x] **压缩包提取**：zip/tar/tar.gz/tgz/tar.bz2/tbz2/tar.xz/txz/gz/bz2/xz
- [x] **PDF 扫描水印检测**：`has_scan_images()` 结构检测替代字符集 Jaccard
- [x] **PDF 图像层 OCR**：pdfimages 提取原始扫描件，绕过 pdftoppm 文字层渲染
- [x] **GBK 文本编码检测**：非 UTF-8 中文文件自动识别
- [x] **Git 版本号**：启动日志和设置页显示 commit hash + 时间
- [x] **日志清空改截断**：写时间戳标记，不破坏 logger 句柄
- [x] **批量索引进度可见**：Phase 1 提取完成即标 indexed=3，UI 实时刷新
- [x] **Phase 1 渐进可搜**：`batch_index` 分块流水线（每 250 文件提交一次），提取的同时已提交部分立即可搜索
- [x] **索引完整性对账**：`check_index_integrity` 命令核对 DB/Tantivy 差异 + 回拨 indexed=3 孤儿；`needs_reindex` 视 3 为未完成
- [x] **浏览页刷新按钮**
- [x] **预览面板高度填充**

---

## P0 — 已知缺陷（应尽快修）

- [x] **PdfExtractor trait impl 硬编码 `"eng"`**：`extract()` 改读全局 `ocr_lang` 设置（OnceLock 缓存 DB 池）
- [x] **LoBackgroundGuard 死代码清理**：struct 已删除
- [x] **Semgrep WARNING**：生产代码 `unwrap`/`expect` 已清零——4 处修复（3 处 nosemgrep 标注修正 + 1 处 webapi 地址解析改优雅降级）；测试代码 494 处保留（AGENTS.md 允许测试路径）（2026-08-25）
- [x] **sherpa-onnx-sys 构建依赖**：已解决——上游 build.rs 原生支持 `SHERPA_ONNX_ARCHIVE_DIR`（本地压缩包）与 `SHERPA_ONNX_LIB_DIR`（解压库目录），无需改代码；README 已补充国内网络构建说明（2026-08-25 核实）

---

## P1 — 性能优化

- [x] **OCR 引擎并发数可配置**：`ocr_concurrent` 设置（默认 2）+ 设置页下拉 → `set_pool_size` 生效
- [x] **IO 竞争缓解**：已实现——`batch_io_pool()` 专用限流 Rayon 池（默认并发 8，`set_batch_io_concurrency` 可调），Phase-1 文件读取不再打满全局池（2026-08-25 核实）
- [x] **启动扫描异步化**：后台线程 + walk 中每 250 jobs 就地 flush 到 `batch_index`，索引与遍历并行推进；删除检测仍在走完树后进行（2026-08-25）
- [x] **浏览页动态分页**：已实现——Browse.tsx ResizeObserver 实测行高动态计算 pageSize（2026-08-25 核实）
- [x] **Tantivy reader 刷新**：已实现——`IndexManager::reader()` 用 `reload()` + 1s 节流，配合 `OnCommitWithDelay` 自动刷新，无 rebuild 开销（2026-08-25 核实）

---

## P2 — 功能增强

- [x] **搜索结果关键词高亮**：预览面板标注命中位置（已实现，SearchPage PreviewPanel）
- [x] **增量扫描剩余时间估算**：scanner 进度日志加 ETA
- [x] **搜索历史前端入口**：SearchBar 下拉 + SearchPage 传 history prop
- [x] **文件类型统计增强**：加索引状态分解（已索引/待处理/失败数）
- [x] **扫描过但不支持的扩展名可见**：扫描时记录白名单外扩展名计数，文件类型页展示（区分缺依赖/不支持）
- [x] **批量导出流式化**：BufWriter 直接写临时文件，不攒内存
- [x] **纯 Rust .doc 解析**：rwml 纯 Rust 解析老格式（已实现于 LibreOffice 移除时；2026-08-25 核实）
- [x] **彻底弃用 LibreOffice 兜底**：已移除全部 soffice 子进程代码与 `lo_binary_path`/`lo_batch_size` 配置，实现零外部依赖——`.doc`→rwml、`.xls/.xlsx`→calamine、`.docx/.ppt/.pptx/.odt/.ods/.odp/.rtf/.epub`→anydoc 全部纯原生（2026-08-11）
- [x] **poppler-utils 零安装**：build.rs 拷贝二进制到 poppler-bin/，运行时查找
- [x] **pdf-inspector 集成**：替代 `has_scan_images()` 图片尺寸启发式判定（四分类+置信度+按页 OCR 路由）
- [x] **损坏文件优雅降级**：classify_error_str 区分加密/损坏/格式不支持
- [x] **索引会话日志**：每次扫描单独日志文件（SessionLogGuard RAII 化 + CLI 补齐，2026-08-25）
- [x] **浏览页多选**：Cmd/Ctrl+单击多选，批量手动索引（已实现于浏览页优化迭代；2026-08-25 核实）
- [x] **`.ods` `.odp` `.rtf` `.epub` 浏览筛选**：前端 filter + 后端路由
- [x] **音频 STT**：FunASR-Nano ONNX 推理，8 种音频格式，Python助手脚本
- [x] **热词增量计数**：jieba 分词 + SQLite，ASR 识别精度增强
- [x] **FunASR 零 Python 化（方案 B）**：sherpa-onnx crate（1.13）替代 Python venv 推理，下载预转 FunASR-Nano ONNX int8 模型（~842MB）；首次使用（模型未就绪）时自动下载，后台进度 + 完成后可立即索引（复用 install_funasr 事件模式）；部署不打包模型、不装 torch，删除 venv/install_funasr 逻辑
- [x] **提取内容质量检测**：对全部文件的正文提取结果做三类检查——①提取内容为空（md5 存在但无文本）②明显乱码（不可打印字符/替换符占比高、无有效中文/英文词）③明显水印文本（"证据"、"仅供"、"附件"等重复水印、极短内容如页码）。检测出的文件标记 quality_bad，支持批量重新提取。接近 P3 的完整性对账 + RAG 内容分析，但独立为可操作项

---

## P3 — 远期规划

- [x] **向量搜索 / AI 增强**：一期 AI 网关+语义搜索（BM25×向量 RRF）；二期 AI 摘要 + 跨文件 RAG 问答（`/chat/completions`）
- [x] **对话式文档检索（多轮 RAG）**：已实现——`conversation_ask_stream` + `prepare_conversation_prompt` 支持每轮重新检索、query rewrite（规则 + LLM 双路）、跨轮 scope 累计、来源去重与证据标注、推理事件时间线（2026-08-25 核实）
- [x] **长文档分块检索**：已实现——索引时按 ~1500 字符 +200 overlap 句界切块存 `doc_chunks` 表（md5 键），RAG 注入对 >50K 文档选 top-8 词法相关段落（带「第X-Y字」标记），扫描后自动回填存量文档（2026-08-25）
- [x] **安全的远程 WebUI 与 API**：可选的 HTTPS 守护进程（axum + rustls，非 Tauri 插件），暴露 70 个 RESTful 路由（搜索/浏览/文件预览/索引状态/扫描/AI 问答/会话 CRUD/备份/日志/事件等），Bearer Token 认证 + 自签名 TLS 证书，默认关闭需显式启用，绑定地址可选 localhost/LAN，设置页可配置端口/Token/绑定地址
- [x] **监控目录热重载**：已实现——update_dir 后自动触发增量扫描 + 重启 watcher（2026-08-25 核实）
- [x] **多语言界面**：中/英/日/韩 UI 完整覆盖，设置页切换（i18n/{zh,en,ja,ko}.ts；2026-08-25 核实）
- [x] **CLI 增强**：已有 `index`(别名 search)、`scan [dir]`、`watch dir`、`health`、`chat`、`quality backfill|audit|reextract` 子命令与全局 `--data-dir`，无 GUI 可用（2026-09-23 核实）
- [x] **RAG 内容分析**：摘要（AI 摘要按钮）、跨文件关联（askDocuments/聊天）、主题聚类（索引状态页 `ai_topic_clusters`，2026-08-25）

---

## P4 — Scope-first RAG 与 Workflow 化（规划中，未实现）

> 目标：用户只提问，不配置字段/模式；**显式 scope 是硬约束**；默认全库找候选、局部读正文；列全/统计走可对账的 scan；流程与 prompt 可配置，核心不变量留在代码。

### 背景与约束

- 当前 AI 链路是 top-30 文件 / 约 140K 字符；`full_recall` 不可靠；无逐文档台账/覆盖率。
- 当前数据：19,914 个 active 文件、19,698 个已索引、16,431 篇 `content_index`、约 1.11 亿字符、71,165 个 chunk；1,314 条抽取失败、27 个不支持格式。
- **不使用 `doc_meta`**：人工维护、文件量大时不可行，也不是完整性/准确性的必要条件。
- 完整性是**相对 scope** 的完整，不是全库完整；准确性靠原文 span 校验，不靠 LLM 置信度。

### 范围（Roadmap）

- [ ] **Scope 模型**：
  - `Explicit`：用户选目录/文件/@mention → 硬过滤，**不自动突破**；
  - `Inferred`：从上下文/实体推断 → 软约束，允许回退；
  - `Global`：未选范围/明确全库 → 全库检索，路径/实体只做 boost，不硬过滤。
- [ ] **TaskSpec parser（LLM）**：输出 `tasks / coverage / granularity / subject / conditions / assumptions / ambiguities`；**不输出 route**，由 planner 推导；无 `has_filters` / `multi_task` 布尔值。
- [ ] **Planner**：内部 task 枚举 `answer | list | count | compare | summarize | check_duplicates`；按 `tasks + coverage + scope` 推导计划；不做每场景 UI 开关。
- [ ] **渐进检索**：`sufficiency_check` + 查询扩展/检索扩展/邻块扩展；最多 2 轮扩展、2 次回答生成、1 次可选 verifier；显式 scope 内不跨范围。
  - 进展（2026-11-01）：`sufficiency` 判定模块 + 稀疏命中 1 轮 BM25 补充检索已落地（默认 `on`，`shadow/off` 可关）；实体/槽位扩展、邻块、回答后校验尚未接入。
- [ ] **Scan 模式**：`scope_snapshot` + 逐文档 ledger + 正则全扫 + LLM 只兜底不确定项；状态含 `processed / no_match / failed / unsupported / not_indexed / stale`；输出覆盖率与缺口。
- [ ] **准确性**：每个条目必须带 `file_id + span_start/end + evidence`；归一化、去重、冲突标记、引用校验；无 span 不进入最终结果。
- [ ] **成本守卫**：全量正则扫描约 1–3 分钟；**禁止全库 LLM 逐篇**（约 8,200 万 token）；推荐“正则全扫 + 5–10% LLM 兜底”（约 400–800 万 token，后台分钟级）。
- [ ] **UI / 日志 / 导出**：scope 徽章、假设、条件精度（精确/近似）、覆盖率、缺口；`ai_events` 记录 `task_spec_parsed / scope_resolved / sufficiency_check / expansion_round / coverage_report`；导出 `run_manifest`。
- [ ] **Workflow DSL + prompt/skill registry**：编排、prompt、阈值可配置；scope 硬约束、ledger 对账、引用校验、预算上限等**核心不变量必须留在代码**；新能力仍需写 skill。

### 分阶段

- [ ] **P4.1 Shadow**：TaskSpec parser + ScopeResolver + Planner 推导，只记录不改变现有行为。
- [ ] **P4.2 全局检索增强**：`Global/Inferred` 路径 boost；`Explicit` 保持硬过滤；本地结果不足仅对 `Inferred/Global` 自动回退。
- [ ] **P4.3 渐进检索**：证据充分性检查、扩展策略、引用/span 校验。
- [ ] **P4.4 Scan 模式**：scope snapshot + 逐文档台账 + 正则全扫 + LLM 兜底 + 覆盖率 UI。
- [ ] **P4.5 Workflow 化**：YAML DSL + prompt/skill registry + golden trace 回归。
- [ ] **P4.6 评估**：61 条真实问题 + 改写扩充到 200–300；看 scope 正确率、引用正确率、列全 false negative、覆盖率、延迟/成本。

### 验收标准

- [ ] **显式 scope 违规次数 = 0**（绝不自动越界）。
- [ ] 引用正确率 ≥ 95%。
- [ ] 列全任务 false negative < 5%（在 gold set 上）。
- [ ] 每次 scan 都有覆盖率/失败数/缺口说明。
- [ ] 普通问答 P95 延迟不显著退化。

### 非目标

- 不做 `doc_meta`；
- 不做“每个场景一个 UI 开关”；
- 不自动突破显式 scope；
- 不默认全库 LLM 扫描；
- 不让 LLM 直接决定最终计划；
- 不承诺任意 OCR 下 100% 抽取。

---

## 📊 本次工作统计（Aug 5-6）

| 类别 | 数量 |
|------|:--:|
| Commits | 30+ |
| 新增模块 | `archive.rs` |
| 重构模块 | `pdf.rs`, `office/mod.rs`, `text.rs`, `indexer.rs`, `logs.rs` |
| 新增依赖 | `anydoc`, `tar`, `flate2`, `bzip2`, `xz2` |
| UI 改动 | Browse 刷新按钮、设置页 AnyDoc 段、版本号显示、预览高度 |
