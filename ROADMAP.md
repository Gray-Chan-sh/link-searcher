# Link-Searcher 路线图

> 最后更新：2026-08-05

---

## P0 — 已知缺陷（应尽快修）

- [ ] **PdfExtractor trait impl 硬编码 `"eng"`**：`pdf.rs:232` 的 `extract()` 里 `extract_with_lang(path, "eng", None)` 不读全局 `ocr_lang` 设置。虽然当前调用链都走 `extract_with_lang`，但作为公共接口是隐藏 bug。
- [ ] **LoBackgroundGuard 死代码清理**：`enter()` 和 `Drop` 已证实无效且修改已签名 app 的 Info.plist 破坏签名。三处调用已移除，但 struct/impl 定义仍在 `office/mod.rs`，应整块删除。
- [ ] **Semgrep WARNING：`unwrap`/`expect` 使用**：完整扫描下存在若干 WARNING 级发现（锁中毒、未处理错误路径），应逐步消除。

---

## P1 — 性能优化（已识别瓶颈，有明确方案）

- [ ] **LO 批次轮询粒度优化**：当前 `poll_lo_outputs` 用 500ms 间隔 + 直接 exec。可改为 200ms 间隔，每文件少等 ~300ms。批次大小从 32 调大也能减少启动次数。
- [ ] **大文件 MD5 优化**：当前 `indexer.rs` 对流式 MD5 已做了首尾 1MB 优化（>100MB 文件），但批量扫描时 `par_iter` 下多文件同时读可能产生 IO 竞争。可考虑 IO 线程池限流。
- [ ] **启动扫描缩短冷启动**：`startup_scan` 目前同步阻塞（lib.rs 线程内串行），此时前端 UI 虽然能显示但无数据。可改为异步分批提交、逐步显示结果。
- [ ] **Tantivy reader 缓存刷新策略**：当前 `IndexManager` 在 commit 后需要手动 rebuild reader。频繁 commit（每 100 文件）可能产生 reader 重建开销。可评估 `IndexReader::reopen()` 替代完全重建。

---

## P2 — 功能增强

- [ ] **搜索结果预览内高亮命中关键词**：当前预览面板只展示全文，无命中位置标注。
- [ ] **PDF 原文高亮渲染**：搜索结果中展示 PDF 页面截图 + OCR 文字对应区域高亮。
- [ ] **增量扫描预计剩余时间**：前端进度条目前只显示 "已处理/总文件"，可基于平均速度估算剩余时间。
- [ ] **搜索历史与收藏**：`search_history` 表已建但前端无入口。可加历史下拉 + 常用搜索置顶。
- [ ] **文件类型统计页增强**：`FileTypes.tsx` 当前只显示扩展名分布。可加各类型索引状态（已索引/待处理/失败数）和体积占比饼图。
- [ ] **纯 Rust .doc 解析**：探索 `doc-rs` 或类似 crate 替代 LibreOffice 处理老式 .doc 二进制格式。成功则可彻底移除 LO 依赖。
- [ ] **批量导出优化**：当前 CSV 导出一次性把全部结果加载到内存。超大数据集（10 万+）可能 OOM。改为流式写入 + 分片。

---

## P3 — 远期规划

- [ ] **向量搜索 / AI 增强**：集成本地 embedding 模型（如 ONNX 格式的 all-MiniLM），支持语义搜索（"找关于合同纠纷的文件"而非关键词匹配）。
- [ ] **监控目录热重载**：目录增删时不需重启扫描。`dir_config` 变更后自动增量同步。
- [ ] **插件化的提取器**：用户可自定义文件格式提取器（Lua/WASM），不依赖 Rust 重新编译。
- [ ] **iOS / Android 只读客户端**：本地网络共享索引数据，手机端只做搜索和预览。
- [ ] **多语言界面**：当前仅中/英文。可扩展日/韩文（jpn/kor OCR 已支持，UI 还缺）。
- [ ] **命令行增强**：`link-searcher index --dir /path`、`link-searcher status` 等 CLI 子命令，支持无 GUI 场景（NAS、服务器）。
- [ ] **索引增量同步 / 备份兼容性**：不同版本间的索引格式兼容校验 + 自动重建。
- [ ] **大模型内容分析与学习**：对已索引文档做 RAG（检索增强生成）、自动摘要、主题聚类、跨文件关联发现。可本地部署（llama.cpp / Ollama）或接云端 API。典型场景："这批判决书的核心争议点是什么""对比这三份合同的差异"。
- [ ] **音频 STT（语音转文字）**：支持 `.mp3` `.wav` `.m4a` `.aac` 等音频格式的语音识别，结果进入全文索引。引擎可选 Whisper.cpp（本地）或 Apple Speech（macOS 原生）。上海话支持需评估模型可用性（Whisper large-v3 有部分吴语识别能力，但上海话专项模型需单独调研）。
