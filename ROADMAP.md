# Link-Searcher 路线图

> 最后更新：2026-08-06

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
- [x] **浏览页刷新按钮**
- [x] **预览面板高度填充**

---

## P0 — 已知缺陷（应尽快修）

- [x] **PdfExtractor trait impl 硬编码 `"eng"`**：`extract()` 改读全局 `ocr_lang` 设置（OnceLock 缓存 DB 池）
- [x] **LoBackgroundGuard 死代码清理**：struct 已删除
- [ ] **Semgrep WARNING**：`unwrap`/`expect` 若干处应逐步消除

---

## P1 — 性能优化

- [x] **OCR 引擎并发数可配置**：`ocr_concurrent` 设置（默认 2）+ 设置页下拉 → `set_pool_size` 生效
- [ ] **IO 竞争缓解**：批量扫描时 `par_iter` 多文件同时读可能产生 IO 竞争，可考虑限流
- [ ] **启动扫描异步化**：当前同步阻塞，可分批提交、逐步显示结果
- [ ] **浏览页动态分页**：根据窗口高度自动计算 `pageSize`（`ResizeObserver`），撑满可视区域，减少翻页次数
- [ ] **Tantivy reader 刷新**：频繁 commit 后 rebuild reader 有开销，可用 `reopen()`

---

## P2 — 功能增强

- [x] **搜索结果关键词高亮**：预览面板标注命中位置（已实现，SearchPage PreviewPanel）
- [x] **增量扫描剩余时间估算**：scanner 进度日志加 ETA
- [x] **搜索历史前端入口**：SearchBar 下拉 + SearchPage 传 history prop
- [x] **文件类型统计增强**：加索引状态分解（已索引/待处理/失败数）
- [x] **扫描过但不支持的扩展名可见**：扫描时记录白名单外扩展名计数，文件类型页展示（区分缺依赖/不支持）
- [x] **批量导出流式化**：BufWriter 直接写临时文件，不攒内存
- [ ] **纯 Rust .doc 解析**：`doc-rs` 等替代 LO 处理老格式
- [x] **poppler-utils 零安装**：build.rs 拷贝二进制到 poppler-bin/，运行时查找
- [x] **pdf-inspector 集成**：替代 `has_scan_images()` 图片尺寸启发式判定（四分类+置信度+按页 OCR 路由）
- [x] **损坏文件优雅降级**：classify_error_str 区分加密/损坏/格式不支持
- [ ] **索引会话日志**：每次扫描单独日志文件
- [ ] **浏览页多选**：Cmd/Ctrl+单击多选，批量手动索引
- [x] **`.ods` `.odp` `.rtf` `.epub` 浏览筛选**：前端 filter + 后端路由
- [x] **音频 STT**：FunASR-Nano ONNX 推理，8 种音频格式，Python助手脚本
- [x] **热词增量计数**：jieba 分词 + SQLite，ASR 识别精度增强
- [x] **FunASR 零 Python 化（方案 B）**：sherpa-onnx crate（1.13.4）替代 Python venv 推理，下载预转 FunASR-Nano ONNX int8 模型（~842MB）；首次使用（模型未就绪）时自动下载，后台进度 + 完成后可立即索引（复用 install_funasr 事件模式）；部署不打包模型、不装 torch，删除 venv/install_funasr 逻辑

---

## P3 — 远期规划

- [x] **向量搜索 / AI 增强**：一期 AI 网关+语义搜索（BM25×向量 RRF）；二期 AI 摘要 + 跨文件 RAG 问答（`/chat/completions`）
- [ ] **监控目录热重载**：`dir_config` 变更后自动增量同步
- [ ] **多语言界面**：日/韩文（jpn/kor OCR 已支持，UI 缺）
- [ ] **CLI 增强**：`link-searcher index --dir` 等子命令，支持无 GUI 场景
- [ ] **RAG 内容分析**：本地大模型做摘要、主题聚类、跨文件关联
- [ ] **音频 STT**：`.mp3` `.wav` 语音识别（Fun-ASR-Nano 支持吴语/上海话）

---

## 📊 本次工作统计（Aug 5-6）

| 类别 | 数量 |
|------|:--:|
| Commits | 30+ |
| 新增模块 | `archive.rs` |
| 重构模块 | `pdf.rs`, `office/mod.rs`, `text.rs`, `indexer.rs`, `logs.rs` |
| 新增依赖 | `anydoc`, `tar`, `flate2`, `bzip2`, `xz2` |
| UI 改动 | Browse 刷新按钮、设置页 AnyDoc 段、版本号显示、预览高度 |
