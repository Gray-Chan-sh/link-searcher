# Link-Searcher 路线图

> 最后更新：2026-08-25

---

## ✅ 本轮已完成

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
- [ ] **Semgrep WARNING**：`unwrap`/`expect` 若干处应逐步消除
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
- [x] **FunASR 零 Python 化（方案 B）**：sherpa-onnx crate（1.13.4）替代 Python venv 推理，下载预转 FunASR-Nano ONNX int8 模型（~842MB）；首次使用（模型未就绪）时自动下载，后台进度 + 完成后可立即索引（复用 install_funasr 事件模式）；部署不打包模型、不装 torch，删除 venv/install_funasr 逻辑

---

## P3 — 远期规划

- [x] **向量搜索 / AI 增强**：一期 AI 网关+语义搜索（BM25×向量 RRF）；二期 AI 摘要 + 跨文件 RAG 问答（`/chat/completions`）
- [x] **对话式文档检索（多轮 RAG）**：已实现——`conversation_ask_stream` + `prepare_conversation_prompt` 支持每轮重新检索、query rewrite（规则 + LLM 双路）、跨轮 scope 累计、来源去重与证据标注、推理事件时间线（2026-08-25 核实）
- [x] **长文档分块检索**：已实现——索引时按 ~1500 字符 +200 overlap 句界切块存 `doc_chunks` 表（md5 键），RAG 注入对 >50K 文档选 top-8 词法相关段落（带「第X-Y字」标记），扫描后自动回填存量文档（2026-08-25）
- [x] **安全的远程 WebUI 与 API**：可选的 HTTPS 守护进程（axum + rustls，非 Tauri 插件），暴露 20 个 RESTful 端点（搜索/浏览/文件预览/索引状态/扫描/AI 问答/会话 CRUD），Bearer Token 认证 + 自签名 TLS 证书，默认关闭需显式启用，绑定地址可选 localhost/LAN，设置页可配置端口/Token/绑定地址
- [x] **监控目录热重载**：已实现——update_dir 后自动触发增量扫描 + 重启 watcher（2026-08-25 核实）
- [x] **多语言界面**：中/英/日/韩 UI 完整覆盖，设置页切换（i18n/{zh,en,ja,ko}.ts；2026-08-25 核实）
- [x] **CLI 增强**：已有 `index`(别名 search)、`scan [dir]`、`watch dir`、`health` 子命令，无 GUI 可用（2026-08-25 核实）
- [x] **RAG 内容分析**：摘要（AI 摘要按钮）、跨文件关联（askDocuments/聊天）、主题聚类（索引状态页 `ai_topic_clusters`，2026-08-25）

---

## 📊 本次工作统计（Aug 5-6）

| 类别 | 数量 |
|------|:--:|
| Commits | 30+ |
| 新增模块 | `archive.rs` |
| 重构模块 | `pdf.rs`, `office/mod.rs`, `text.rs`, `indexer.rs`, `logs.rs` |
| 新增依赖 | `anydoc`, `tar`, `flate2`, `bzip2`, `xz2` |
| UI 改动 | Browse 刷新按钮、设置页 AnyDoc 段、版本号显示、预览高度 |
