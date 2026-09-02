# Link-Searcher 变更日志

> 2026年7月30日 — 8月31日，共 50+ commit，修复 85+ Bug，完成 40+ 功能改进

---

## 2026-09-02（AI 问答调研）

- **研究：面向人少+文档量小场景的 AI 问答最佳实践调研报告**：深度调研个人/小团队本地文档 RAG 的业界做法（Anthropic Contextual Retrieval / Context Engineering、Pinecone chunking & hybrid search、Faiss 索引选型、ZeroEntropy LLM-reranker 基准、The Reranking Trap、Neel Mishra citation grounding、RedHop RAG citations、RAGAS 评测、luningqi 混合检索实测、BGE-M3/reranker 模型卡等 22 个经全文核实来源），并逐条对照本项目真实代码（`commands/ai.rs` 单体 RAG 管线、`chunks.rs` 分块、`tracker.rs` 向量存取、`ChatPanel.tsx` 引用渲染、`webapi` 事件桥），产出差距分析与分优先级实施路线图。交付：`docs/research-rag-best-practices-personal.md`（新增第 9 章差距分析 + 第 10 章 P0/P1/P2 路线图 + 明确不做清单）。核心结论：混合检索/小文件不切块/引用编号/范围控制已符合业界共识；主要差距在 P0（0 命中/低置信硬答、幻觉引用未白名单剥离、长文档静默截断）、P1（向量暴力余弦+无查询缓存、文件级向量只编前 2000 字符、Web 无 ai-progress）、P2（**无评测基线**、ai/skills 双份实现漂移、chunk 无结构语境）。本次仅文档调研，无代码改动。
- **调研报告按真实库规模修正**：用户澄清实际监控库达 2.2 万文件、总 2.65 亿字、单文档最长 667 万字后，实测各库分布（com.link-searcher.app：22,441 active / 最长 667 万字，75% ≤1 万字；/Volumes/Data/index：doc_chunks 12.5 万行但 chunk_embeddings 仅 1154，回填滞后 ~100 倍），确认推理环境为本地 MLX（LLM Qwen3.6-35B、生效 embedding 为 local:bge-large-zh-v1.5）。原报告"1 万级向量、全表线性扫描够用、别上 ANN"的结论在 chunk 12.5 万–50 万+ 下不再成立。**修正**：`docs/research-rag-best-practices-personal.md` 第 9.1 规模参照更新为实测值 + 顶部新增规模修正警示区块；第 10 章 P1 重构为「规模适配」——新增 **P1-A 两级检索漏斗**（文档级粗筛→命中块精检，替代全库 chunk 暴力/ANN）、**P1-B 块级懒嵌入**（解决 100 倍回填滞后）、**P1-C doc 向量驻留内存 + 查询缓存**（chunk 不常驻）、**P1-D 结构感知切分上提**（5 篇 >1M 文档必需）；原 P1-2（文件级向量扩展）降级为不做（ROI 低，被 P1-A/B 覆盖）；🚫 不做清单与 10.5 门禁同步更新（全库 chunk 暴力扫描移入不做清单）。本次仍仅文档修正，无代码改动。

---

## 2026-09-01（检索修复）

- **AI 聊天检索不到人名/实体（如"涉及常宏的民事案件"答"找不到"）**：完整问句直接进三路检索，核心实体被泛词淹没。**根因**：(1) `path_match_files` 用完整问句做 `LIKE %...%` → 必然 0 命中（本应命中 5559 份路径含"常宏"的文件）；(2) BM25 把问句分词为 `涉及 OR 常宏 OR 民事 OR 案件 OR 民事案件 OR 一共 OR 多少 OR 列表`，泛词稀释 BM25 分数，"常宏"文件被挤出注入前 30，前 30 全被"民事案件案由规定"等无关高分文档占据；(3) 向量通道同样用完整问句，11684 个向量 0 命中。**修复**：新增 `extract_retrieval_keywords`（jieba Search 模式分词 + 检索级停用词表），从问句提炼核心实体词（"常宏"），BM25/向量/路径三通道改用实体词检索——`path_match_files` 改为多关键词 OR 匹配；无实体时回退完整问句。涉及 `src-tauri/src/commands/ai.rs`、`src-tauri/src/db/tracker.rs`，新增 6 个单元测试。验证：修复前注入前 30 全无关文档 → 修复后前 30 全是"常宏"相关文件（起诉书/会见记录/判决书/质证意见），命中从 8007 收敛到 5954。
- **AI 聊天命中多时卡 1 分钟+ 不回复**：命中 >30 份时无条件对 60 份跑 LLM 摘要兜底（`batch_summarize`，4 批 × 15 + 1 次整合 = 5 次本地 Qwen 调用 ≈ 48s），且完全阻塞回答。**修复**：移除 Layer 2 摘要兜底（`ai.rs`），命中>30 时直接靠前 30 全文注入回答。删除 `batch_summarize` 函数。验证：dry-run 检索从 1m46s 降到 12.5s，命中 5957 份时不再触发 LLM 摘要。
- **向量检索重复嵌入查询（debug 下翻倍浪费）**：`vector_full_scan` 与 `chunk_vector_scan` 各自内部调一次 `embed(query)`，同一查询被嵌入两次——debug 下 bge-large 单次推理 85s，两次共 118s。**修复**：拆分出 `vector_scan_with_query_emb` / `chunk_vector_scan_with_query_emb` 接收预计算查询向量，`ai.rs` 只 embed 一次两通道共享。验证：debug dry-run 从 2m33s 降到 1m5s（命中不变 5957），release 14s。
- **文档**：`docs/08-ai-features.md` 补充「无引用提问」与「有引用提问」两张 Mermaid 流程图 + 差异对比表；移除已删除的「摘要兜底」过时描述（截断表 #4）。
- **大量引用时静默丢弃引用文件（正确性修复）**：Layer 0 引用注入原本每个文件 `chunked_or_truncated` 全文（≤50k），拼接后 `truncate_text(50k)` 一刀切——前面的引用文件占满预算、**后面的引用文件被静默丢弃**，strict 模式也不报错。**修复**：Layer 0 改**均摊预算**——所有引用文件共享 `CONTEXT_BUDGET/3`，每文件按份数分配（`chunked_or_truncated_with_budget`），超长文件取相关块而非全文，保证每个引用文件都注入一部分。同时 Layer 1 的 content_budget 不再重复扣除 mention 预算（原预留 50k 已被 Layer 0 自管）。
- **注入顺序统一按相关性排序（商用 RRF 一致性）**：三路合并的 `all_hits` 原本按通道添加顺序排列（BM25 在前、向量次之、路径最后），即使向量命中相似度更高也排后面。**修复**：合并后统一按混合分排序（`w×cosine + (1-w)×bm25_norm`，w=semantic_weight），BM25 分与语义分分别归一化，路径命中（无分）排最后——最相关的文件先进注入前 30。
- 新增 `chunked_or_truncated_with_budget` 4 个单元测试（预算截断/短文本全文/零预算/语义命中块优先）。

---

## 2026-08-31（Bug 修复）

- **「缩小范围」后「去聊天」只传 20 个文件**：搜索页 `goToChat`（`SearchPage.tsx:174-180`）从 `currentRefine.hits`（仅 20 条分页片）反查 path，丢掉了 `allFileIds` 中已选但不在当前页的 ID 的 path。**根因**：`enterRefine` 把全量 ID 存进 `allFileIds`（来自 `search_file_ids_only`，page_size=5000），但只有 ID 无 path；`goToChat` 拿不到 path 时只能回退到当前页 20 条 hits。**修复**：把 `search_file_ids_only` 的返回从 `Vec<String>` 升级为 `Vec<{file_id, path}>`（`search.rs` 新增 `IdWithPath`），前端 `allFileIds` → `allFileRows: IdWithPath[]`，`goToChat` 用 Map 按 ID 反查全量 path，30 个选中就传 30 个到 `ls_pending_chat_paths`。涉及 `src-tauri/src/commands/search.rs`、`src/api/search.ts`、`src/pages/SearchPage.tsx`，新增 1 个序列化单测锁定 wire shape。
- **顺手修 `ls_pending_chat_query` 死代码**：`SearchPage.tsx` 一直把搜索 query 写入 sessionStorage 但 `AiChat.tsx` 只 `removeItem` 从不读取，导致新会话 `pending_query` 永远是 null。改为 `AiChat.tsx:104-134` 真正读出后赋给 `pending_query` + `pending_started_at`，让「从搜索跳到聊天」的首轮能携带原始搜索意图。
- **AI 聊天导出文件名出现 `.*` 后缀**：`saveFile` 中 Tauri `extensions: ['*']` 硬编码导致 macOS NSSavePanel 追加后缀；改为从 `defaultName` 提取扩展名（与 `exportFile` 一致），`platform.ts`
- **AI 回答中数字被标注打断（如 `32.[3][1]5万元`）**：`auto_cite` 句子分割正则把 `.`（小数点）误作句终符；移除 `.` 从断句字符集，中文文本以 `。！？` 为句终，`ai.rs` + 新增单元测试
- **缩小范围后证据面板只显示部分文件**：Layer 1（BM25 命中文件）注入 prompt 时未同步向 `evidence` 数组添加条目，导致前端溯源面板不显示这些文件；修复后每份注入文件均生成对应 `EvidenceItem`，`ai.rs`

---

## 2026-08-31（性能优化）

- **AI 上下文注入 N+1 查询**：`prepare_conversation_prompt`（`ai.rs`）对每个 hit 逐条调 `get_file_by_id` + `get_content`，30 个文件 = 60+ 次 DB round trip。新增 `tracker.rs` 批量版 `get_files_by_ids` / `get_contents`（`IN (?,?...)` 单次查询），`ai.rs` 三处热循环（旧来源补入、Layer 0 mention 注入、Layer 1 BM25 注入）改为预取到内存 Map 后查表。`smart_search` 路径同改。
- **`auto_cite` 正则编译在循环内**：`ai.rs:1399` 对每个句子 `Regex::new(r"\[\d+\]")`，500 句编译 500 次。提升到循环外，与 `code_block_re`、`sent_re` 同级。
- **`summarize_file` 静默吞写库错误**：`let _ = upsert_summary(...)` 改为 `if let Err(e) = ... { log::warn!(...) }`，避免摘要写入失败时无任何日志。
- **ChatPanel 每秒 re-render 整个面板**：`setInterval(() => setClockNow(Date.now()), 1000)` 让 756 行组件（消息流、chips、mention 状态）每秒全量 diff。提取为独立 `ElapsedTimer` 小组件，只有计时文本 re-render。
- **TypeScript `noUncheckedIndexedAccess`**：`tsconfig.app.json` 开启，暴露 27 处 `arr[i]` 潜在 undefined（`ChatPanel`、`SearchPage`、`Browse`、`scopeParser` 等），全部加 `!` 或 guard 修复。
- **Clippy 自动修复**：`cargo clippy --fix` 修 20+ 条（`div_ceil`、`is_empty`、`rfind`、`is_multiple_of`、`clamp`、`is_err`、`let...else → ?`），剩余 11 条为结构性警告（函数参数过多），留待后续重构。
- **Settings.tsx 拆分（1493 行 → 311 行）**：按 tab 拆成 `src/components/settings/` 下 7 个文件（SettingsFields 通用组件 + GeneralTab/DocsTab/IndexTab/AiTab/BackupTab/SystemTab），state 与 handlers 拆到 3 个 hooks（useSettingsOcr/useSettingsBackup/useSettingsProviders）。主组件只保留 tab 切换与 props 分发，可读性与可维护性大幅提升。
- **Chunk 级语义检索（解决长文档细节召回）**：原文件级嵌入只嵌前 2000 字符，长文档细节（如"违约金 8%"在 8000 字符处）在向量中无信号，BM25 top-K 也常被挤掉。新增 `chunk_embeddings` 表（md5, chunk_index → 向量），长文档每 ~1500 字符块独立嵌入；检索升级为双通道（文件级 `vector_full_scan` + chunk 级 `chunk_vector_scan` 并集去重），命中块经 `ScoredHit.hit_chunks` 带入注入层优先注入。涉及 `db/mod.rs`（建表）、`db/tracker.rs`（CRUD + get_files_by_md5）、`commands/index.rs`（回填 + 命令 + 清理）、`ai/mod.rs`（chunk_vector_scan）、`commands/ai.rs`（双通道检索 + 注入块优先 + ScoredHit 扩展）、`lib.rs`（命令注册 + 启动触发）。新增 4 个单测（CRUD roundtrip / files_by_md5 / chunk_scan 过滤排序截断）。

---

## 2026-08-31（文档 — 设计手册重写为分模块教程）

- **`docs/ARCHITECTURE.md`**：彻底重写为**分模块设计手册**，面向技术小白，著重说明思想、架构、目标。14 个章节覆盖：
  - 一、总体设计思想（核心使命、三个核心问题、复现七步路线图）
  - 二至十一、十大模块（全文搜索/提取管线/扫描监控/AI出口/语义检索/RAG管线/会话事件/WebAPI/SQLite/前端），每模块按「解决什么问题 → 为什么这样设计 → 核心架构 → 复现要点 → 边界取舍」展开
  - 十二、模块间数据流总览
  - 十三、核心设计原则
  - 十四、已知取舍与未来方向
- **README.md**：新增「文档导航」区块，链接用户手册/设计手册/搜索UX实现
- 每个模块含设计思想图解、Rust/TS 复现要点代码、以及为何如此取舍的解释

---

## 2026-08-31（文档 — 用户手册 + 开发者架构文档）

- **`docs/08-ai-features.md`**：新增「全量召回与内容截断」章节，说明 AI 问答中文档内容被截断/丢弃的 6 种情况、`full_recall` 开关的作用范围，以及引用文档与 BM25 命中在注入策略上的区别
- **`docs/ARCHITECTURE.md`**（新增）：面向开发者的完整架构文档，涵盖 AI RAG 管线架构、核心数据结构、三种请求路径（Tauri IPC / Web API / CLI）、内容注入三层策略、事件系统、Web API 服务器、调试诊断指南、10 项已知缺陷与设计约束

---

## 2026-08-31（修复 — Web 模式「推理过程」面板无内容）

**根因**：`ai_events` 表里推理时间线数据完整（query_rewrite → scope_resolved → retrieval → context_assembled → llm_call → turn_complete 6 类事件），但 **Web API 模式的查询链路缺失**：后端 Web API 层没有暴露 `ai_events` 的读取端点（只有 Tauri IPC 命令 `get_ai_events`/`get_turn_ai_events`），前端 `client.ts` 的 MAPPINGS 也没有这两条命令的 HTTP 映射。于是浏览器模式下调 `getTurnAiEvents()` 时，`client.invoke` 命中 `if (!entry)` 分支（`[client] No mapping for "get_turn_ai_events"`），直接返回空数组，`AiEventTimeline` 因 `events.length === 0` 而渲染空白。桌面（Tauri IPC）模式不受影响。

**修复**：
- `src-tauri/src/webapi/routes/ai.rs`：新增两个端点
  - `GET /api/ai/events?session_id=…` → `get_session_events`（会话全部事件）
  - `GET /api/ai/events/turn?session_id=…&turn_number=N` → `get_turn_events`（单轮事件）
- `src/api/client.ts`：MAPPINGS 补充 `get_ai_events`、`get_turn_ai_events` → 上述端点

**顺带修复（检索行显示 `bm25=undefined`）**：`src/components/AiEventTimeline.tsx` 的 `retrieval` 分支读的是 `bm25_hits`/`merged_hits`，但实际事件 payload 的键是 `total_matches`/`from_history_count`，字段错位导致显示 `bm25=undefined · merged=undefined`。改为读取 `total_matches`（带 `merged_hits` 兜底），显示为 `matched=N`。

**验证**：`cargo check`、`tsc -b` 零错误；重启 debug 后端、重建 dist 后，浏览器中「概括那个压缩包的内容」会话的推理过程面板完整显示 6 条事件（查询改写/范围解析/检索/上下文组装/LLM 调用/轮次完成），检索行显示 `matched=0` 而非 `undefined`。

---

## 2026-08-31（修复 — WebUI 模式 AI 回复被前端丢弃）

**根因**：`src/components/ChatPanel.tsx` 的「恢复挂起请求」effect 依赖数组里含有 `pending_query`。每次 `handleSend` 设置 `pending_query` 后，该 effect 立即把刚写入的 `pending_started_at` 清回 `null`，导致 `loadingRef.current` 恒为 `false`。4–5 秒后 `ai-done` 事件到达时，被其回调里的 `if (!loadingRef.current)` 守卫丢弃（`console.warn('ai-done dropped: loadingRef is false')`），AI 回复既不在 UI 渲染、也不落库，用户只看到问题发出却无任何回答/报错。

**修复**：`ChatPanel.tsx` —— 将该 effect 依赖数组由 `[session?.id, session?.pending_query]` 改为 `[session?.id]`，使它在同会话内发送时不触发清空，仅切到另一会话（残留 pending）时才清理。保留了「切会话清残留」意图，同时恢复 `loadingRef` 在请求期间的 `true` 状态。

**验证**：WebUI（Web API 模式）完整走「新建搜索 万城(1303条) → 🔍缩小范围 在结果内搜常宏案(→1份) → 去聊天 → 提问」：修复后 AI 回答正常渲染（含 `⏱耗时`、`🔍检索依据`、`🧠推理过程`），且 `per_turn_evidence` 正确写入 `chat_history.json`。修复前该流程回答既不渲染也不落库。

> 本次未新增组件测试：ChatPanel 的 bug 属 effect 依赖时序问题，难以抽成纯函数，且项目现有测试体例为纯工具函数测试（`src/utils/__tests__/*`），故维持不引入 jsdom/testing-library 依赖。

---

## 2026-08-30（修复 — AI 对话无响应）

**根因**：两个问题导致 AI 问答静默失败。

1. **LLM 网关 API key 过期**：`192.168.1.50:20128` 拒绝了旧 key，`chat()`/`chat_stream()` 返回 `None`，前端收不到 `ai-done` 事件。
2. **`batch_summarize` 无上限**：BM25 命中 2000+ 份时，`remaining_hits` 全部送去摘要，发起上百个并行 LLM 调用，耗时数分钟。

**修复**：
- `commands/ai.rs`：`remaining_hits` 增加 `.take(MAX_REMAINING_SUMMARY)`（上限 60 份），避免大量命中时 LLM 调用失控
- `commands/ai.rs:1148`：`all_hits.len() - bm25_count` 改为 `saturating_sub`，修复 debug 模式下整数下溢 panic
- `commands/ai.rs`：`prepare_conversation_prompt` 关键路径添加诊断日志（prepare 成功/失败、context 规模、chat_stream 进出）


**动机**：法庭录音等对话类音频文件需要区分说话人（谁说了什么），方便检索和理解。

**方案**：在现有 FunASR-Nano 语音识别之上，增加 sherpa-onnx 原生说话人分离管线：
- **分段**：Pyannote segmentation（`seg.onnx` 5.7MB）检测说话切换点
- **嵌入**：CAM++ 中文说话人嵌入（`emb.onnx` 27MB）提取每位说话人特征
- **聚类**：FastClustering 自动聚类说话人
- 对每个说话人片段独立转写，输出 `[说话人N] 文本` 格式

**变更**：
- `extractor/audio.rs`：新增 `SPEAKER_DIARIZER` 静态初始化器；`OfflineSpeakerDiarization` 管线（pyannote 分段 + CAM++ 嵌入）；`diarization_dir()` 解析自 `funasr_dir()/models/diarization`；逐说话人片段转写并标注 `[说话人N]`
- `models/funasr/sherpa-onnx-funasr-nano-int8-2025-12-30/models/diarization/`：新增 `seg.onnx`（pyannote 分段）+ `emb.onnx`（CAM++ 中文），从 hf-mirror 下载
- `tests/funasr_smoke.rs`：新增 `audio_extractor_long_audio` 测试验证 VAD 分段转写；修复 UTF-8 边界 panic
- 音频大小限制从 30 分钟提升到完整长度（去掉 `-t 1800`）
- 日志格式优化：时间戳缩写、模块名取最后一段、过滤 tantivy/tract 噪音、embed/vector 日志去重

**验证**：3.3 分钟双人对话测试音频输出正确区分说话人 `[说话人3]喂[说话人1]哎彭老师啊...`

---

## 2026-08-28（功能 — 搜索页缩小范围 → AI 聊天一键引用）

**动机**：搜索只管搜索/缩小范围，AI 聊天在独立页面。用户在搜索页反复子搜索缩小文件集，锁定后一键跳转 AI 聊天页，自动创建会话并以锁定文件为检索范围。

**方案**：搜索页 Drawer 只做缩小（子搜索 + 勾选），底部按钮「锁定 N 篇，去聊天」将 paths 存入 sessionStorage 并 `navigate('/chat')`。AiChat 挂载时检查 sessionStorage，若有 pending paths 则自动 `createChatSession` + 设置 `retrieval_scope = paths` + `strict_docs = true`。

**变更**：
- `commands/search.rs`：新增 `search_file_ids_only`（全量 file_ids，不分页）+ `refine_search`（BM25 子集检索）
- `lib.rs`：注册 `search_file_ids_only` + `refine_search`
- `api/search.ts`：新增 `searchFileIdsOnly`、`RefineSearchResponse`、`refineSearch`
- `api/client.ts`：新增 web 模式 HTTP 映射
- `SearchAskPanel.tsx`：重写为纯缩小面板（无 AI 对话），Props 新增 `onLockAndAsk(paths)` 回调
- `SearchPage.tsx`：按钮点击 → `searchFileIdsOnly` 获取全量 ID → 打开 Drawer；锁定后存 sessionStorage + `navigate('/chat')`
- `AiChat.tsx`：新增 effect，挂载时读取 `ls_pending_chat_paths`，自动创建会话并设置 scope
- 移除 `useSearchAsk.ts`、`ask_search_results_stream` 命令及相关死代码
- `i18n/zh.ts` + `i18n/en.ts`：更新 `lock_and_ask` 文案为「去聊天」

---

## 2026-08-28（功能 — Web API 开发模式：代理到 Vite dev server）

**动机**：通过 WebAPI（8443 端口）远程调试前端时，修改代码后需手动 `npm run build` 才能生效，无法热更新。

**方案**：Settings → Web API 区域新增「开发模式」开关。开启后，WebAPI 的静态文件请求代理到 Vite dev server（`localhost:1420`），改代码后浏览器自动热更新。使用 `ureq`（已有依赖）+ `spawn_blocking` 做同步 HTTP 代理，不引入新依赖。

**变更**：
- `commands/settings.rs`：`ALLOWED_KEYS` 新增 `web_api_dev_mode`
- `webapi/state.rs`：`ApiState` 新增 `dev_mode: bool` 字段
- `webapi/mod.rs`：新增 `KEY_DEV_MODE` 常量 + `load_dev_mode()` 函数，`spawn_server` 读取并传入 `ApiState`
- `webapi/static_files.rs`：`serve_static` 新增 `State<ApiState>` 参数，`dev_mode` 时代理到 `http://127.0.0.1:1420`，否则走原 `dist/` 逻辑
- `Settings.tsx`：Web API 区域新增「开发模式」ToggleField

---

## 2026-08-28（功能 — 搜索页「基于结果提问」：先搜后问，严格锁定范围）

**动机**：用户想先在搜索页看到结果、确认命中文件，再以这批文档为依据向 AI 提问，实现"先搜索、再生成"的 RAG 场景。

**方案**：搜索结果区新增「✦ 基于结果提问」按钮，打开右侧 Drawer 面板；把当前搜索命中的 file_ids 作为初始 scope 锁死传入后端，所有后续追问均在该 scope 内执行 BM25/语义重排，不自动扩展范围（strict_docs=true）。每轮可换 query 缩小命中文档，evidence 仅含当轮命中子集，query 链实时展示在顶部 chip 中。

**变更**：
- `commands/ai.rs`：新增 `ask_search_results_stream` 命令，接收 `initial_file_ids`（搜索结果 file_ids）+ `question` + `session_id`，将 file_ids 解析为路径后构建锁定的 `TurnScope`，调用 `conversation_ask_stream` 并设 `strict_docs=true`
- `lib.rs`：注册 `ask_search_results_stream`
- `api/search.ts`：新增 `askSearchResultsStream` + `listenSearchAskStream` 封装
- `api/client.ts`：新增 `ask_search_results_stream` 的 web 模式 HTTP 映射
- `hooks/useSearchAsk.ts`：新建流式 hook，管理多轮对话状态、streaming delta、evidence、取消
- `components/SearchAskPanel.tsx`：新建右侧 Drawer 组件，顶部 chip（🔒 严格依据 · query 链），消息气泡 + evidence 展开面板，输入框支持多轮追问
- `pages/SearchPage.tsx`：搜索结果区新增「✦ 基于结果提问」按钮（仅 LLM 可用且有结果时启用）；开启后显示 SearchAskPanel 替代 PreviewPanel；关闭时恢复预览面板
- `i18n/zh.ts` + `i18n/en.ts`：新增 `ask_from_search`、`strict_scope_locked`、`search_query_chain`、`close_ask_panel`

---

## 2026-08-28（功能 — AI 聊天检索阶段进度条：提问后到 LLM 回答前的进度可见）

**动机**：用户提问后到 LLM 开始回答前的检索+注入+摘要阶段（全量召回时可能几十秒到几分钟）前端完全黑盒，用户无法判断是否卡死。

**方案**：后端在 `prepare_conversation_prompt` 和 `batch_summarize` 各阶段 emit `ai-progress` Tauri 事件（phase/message/current/total），前端监听并在 loading 状态显示阶段文本 + 进度条。第一个 `ai-chunk` 到达时自动清空进度（进入 LLM 回答阶段）。

**变更**：
- `commands/ai.rs`：新增 `AiProgress` 事件结构（session_id/phase/message/current/total）；`prepare_conversation_prompt` 加 `app: Option<&tauri::AppHandle>` + `session_id: &str` 参数，在查询改写/BM25/向量扫描/路径匹配/内容注入/摘要各阶段 emit 进度；`batch_summarize` 同样加参数，每批完成后 emit 进度
- `commands/ai.rs`：`conversation_ask_stream` 传 `Some(&app)` + `&session_id`；`conversation_ask` 传 `None` + `""`（非流式无进度）
- `api/files.ts`：新增 `AiProgressPayload` 类型 + `listenAiProgress` 函数
- `ChatPanel.tsx`：新增 `progress` 状态 + `ai-progress` 事件监听 effect；loading 状态渲染阶段文本 + 进度条（`████░░ N/total`）；`ai-chunk`/`ai-done` 到达时清空 progress
- `commands/ai.rs`：`prepare_conversation_prompt` 加 `check_cancel!` 宏，在 BM25 前、注入循环每 10 文件、batch_summarize 前检查 `ai_cancelled()`；`batch_summarize` 每批 join 后检查——修复取消功能在检索/摘要阶段完全无效的问题
- `ChatPanel.tsx`：恢复挂起请求 effect 加 10 分钟新鲜度判断——程序重启后残留的 `pending_query`/`pending_started_at` 超过阈值直接清除，避免"关闭重开后永久思考中"
- `ChatPanel.tsx`：取消按钮去掉 `confirm()` 弹窗（取消是安全操作，无需二次确认）；保留已输出的部分流式文本并标注"已取消"；无流式文本时也插入"⏹ 已取消"提示消息；清除 progress 进度条；`handleCancel` 重置 `sendingRef.current = false`；`handleSend` catch 块在 reqId 检查前先重置 `sendingRef`——修复取消后无法再次发送的问题
- `commands/ai.rs`：`conversation_ask_stream` 在 LLM 调用前 emit `ai-progress`（phase=`llm_call`，"等待 AI 回答中..."），填补检索完成到第一个 `ai-chunk` 到达之间的进度空白

---

## 2026-08-27（功能 — AI 聊天「全量召回」开关：检索不截断，回答不遗漏文件）

**动机**：AI 聊天在大库下仍有三重召回瓶颈——向量扫描 500 条硬截断、`MAX_CONTENT_INJECT=30` 全文注入上限、注入预算均分导致高相关文件拿不到足够篇幅。部分文件被静默丢弃。

**方案**：新增会话级「全量召回」开关（默认开启）。开启时检索不截断（向量全量+BM25 全量+路径匹配），注入改为"预算内从高相关到低相关递减分配、预算耗尽即停"，上下文截断上限从 50K 提至预算上限；未注入文件仍走 `batch_summarize` 摘要兜底，保证范围内所有文件都被 AI 看到。

**变更**：
- `commands/ai.rs`：`prepare_conversation_prompt` 新增 `full_recall` 参数；向量扫描 `if i>500 break` 改为 `!full_recall 时`才截断；`MAX_CONTENT_INJECT` 全量时改 `all_hits.len()`（由 `content_budget` 自然限制）；层2注入由"均分预算"改为 full_recall 时"递减预算"（`content_budget` 递减，耗尽即 `break`）；层3 `skip` 同步改为 `skip(inject_limit)`（防止已注入文件重复摘要）；`context` 最终截断上限 full_recall 时提至 `CONTEXT_BUDGET - SYSTEM_OVERHEAD - ANSWER_RESERVE`；删除死常量 `MAX_CONTENT_INJECT=30`
- `commands/ai.rs`：`conversation_ask` / `conversation_ask_stream` IPC 新增 `full_recall: Option<bool>` 参数
- `webapi/routes/ai.rs`：`AskBody` 新增 `full_recall` 字段，两个 handler 透传
- `api/files.ts`：`ChatSession` 新增 `full_recall` 字段；`conversationAsk`/`conversationAskStream` 签名加 `fullRecall` 参数
- `ChatPanel.tsx`：新增「全量召回」紫色 toggle（与「仅依据文档」同行），随会话持久化；两个 IPC 调用点传 `session.full_recall`
- `AiChat.tsx`：新会话初始化加 `full_recall: true`
- `i18n/`：四语言新增 `full_recall` 文案
- `src-tauri/src/cli.rs`：新增 `Chat` CLI 子命令（`link-searcher chat <QUERY> --full-recall`），BM25 检索结果直接打印，验证全量召回生效（11K 库中"陈骥"命中 5482 vs 默认 89）；`main.rs` 转发 `--data-dir <path>` 到 CLI；`config.rs` 支持 `LINK_SEARCHER_DATA_DIR` 环境变量
- `src-tauri/src/commands/ai.rs`：`prepare_conversation_prompt`/`bm25_relevant_hits`/`batch_summarize` 参数从 `&tauri::State` 改为 `&AppState`（CLI 可直接调用）；`PreparedConversation` 字段改为 `pub(crate)`

---

## 2026-08-25（功能 — 大范围 AI 聊天：三路全量扫描 + Map-Reduce，不限文件数量）

**动机**：用户需要统计所有与某案件相关的文件内容，现有 BM25 limit=10 + 2000 字符/文件 + 50000 总上下文的三重天花板导致大量相关文件被丢弃。

**方案**：BM25 全量扫描 + 向量全量扫描（暴力 cosine）+ SQL 路径匹配 → 三路合并 → 动态预算注入 / Map-Reduce 批量摘要

**变更**：
- `ai/mod.rs`：新增 `vector_full_scan()` — 遍历 `doc_embeddings` 全表计算 cosine，阈值 0.65 过滤，无 top-K 限制
- `db/tracker.rs`：新增 `path_match_files()` — SQL LIKE 路径匹配，无上限；新增 `count_active_files()` — BM25 全量扫描的 limit 参数
- `commands/ai.rs`：
  - 新增 `chunked_or_truncated_with_budget()` — 预算感知分块注入，贪心打包直到预算耗尽
  - 新增 `batch_summarize()` — Map-Reduce 批量摘要：分批 ×15 LLM 调用（并发）+ 1 次 reduce
  - `prepare_conversation_prompt` 重构：BM25 limit 10→全量 + 向量全量扫描 + SQL 路径匹配；`MAX_SOURCES=15` 移除；证据列表包含全部匹配文件；动态预算分配
- `api/files.ts`：`AiDonePayload` 新增 `total_match_count` 字段
- `ChatPanel.tsx`：证据面板显示匹配总数/注入数（如 `🔍 证据（30/200）`）

## 2026-08-26（性能 — 索引全链路优化 W1-W5：SQLite + 写路径 + 正确性 + Tantivy + 前端）

**动机**：路线图 P1 性能优化。通过 5 波并行探索 + Plan Agent 任务图，对索引管线全链路进行系统性优化。

**W1 — SQLite/pragmas/死代码**（`48e8ba1`）：
- `db/mod.rs`：新增 `synchronous=NORMAL`（WAL 标准优化，跳过 WAL fsync）+ `cache_size=-8000` + `temp_store=MEMORY`；pool `max_size(32)` → `12`（匹配 IO 并发度）；`init_db` 迁移后运行 `PRAGMA optimize`；`CREATE_TABLES_SQL` 新增 `ai_events.created_at` 索引
- `indexer.rs`：移除死代码 `update_hotwords` 调用（audio.rs 已标记 disabled，per-file jieba 分词+逐词 INSERT 是最大隐藏写 N+1）；Tantivy writer 预算 `50MB` → `150MB`；CHUNK `250` → `500`
- `extractor/mod.rs` + `text.rs`：`Vec::with_capacity(10MB)` → `Vec::new()`（未知文件不再立即分配 10MB）
- `paddleocr.rs`：EnginePool `2` → `clamp(available_parallelism, 1..8)`（与 OcrGate 对齐，不再有 6 个线程等待 2 个引擎 Mutex）

**W2 — DB 写路径事务化**（`26b886e`）：
- `replace_chunks`：DELETE + INSERT 包装在 `unchecked_transaction` 中，崩溃一致性修复
- Phase-2 `update_indexed`：新增 `update_indexed_batch` 批量事务函数，每 chunk 250 文件从 N 次 autocommit → 1 次 commit
- `run_backfill_chunks`：`length(text_content)` → `ci.char_count`（避免每次回填加载全部内容 blob）
- `get_total_image_files` + `get_ocred_count`：7× 循环 LIKE 查询 → 单次 `file_ext IN (...)` 查询
- `CREATE_TABLES_SQL`：新增 `idx_ft_pending` 部分索引（WHERE indexed IN (0,2,3)），加速核心索引队列查询

**W3 — 正确性修复**（`b6eb826`）：
- `pdf.rs`：`pkill -f pdftoppm/pdfimages` → 轮询 `child.try_wait()` + `child.kill()` 超时杀死特定子进程（修复系统级进程误杀 bug）

**W4 — Tantivy 优化**（`b6eb826`）：
- `search/indexer.rs`：`build_schema()` 改用 `static OnceLock<Schema>` 缓存（消除每次 add_document/delete_document 的冗余 Schema 构建）

**W5 — 前端 UX**（`10d146d`）：
- `IndexStatus.tsx`：从 `scan-progress` 事件实时读取 processed/total（而非 5s 轮询），进度条即时更新；`retryFailed` 未 await 修复
- `ResultList.tsx`：scroll handler 添加 `requestAnimationFrame` 节流（消除每帧渲染）
- `useSearch.ts`：`suggest()` 添加请求 ID 防竞态（快速输入时旧结果不再覆盖新结果）

**W2c 跳过**：scanner walk upsert 批量事务（风险高，现有 `FLUSH_EVERY=250` + Phase-1 隔离已覆盖）

**审计**：cargo test 202 全绿 + semgrep ERROR 0 + clippy 警告仅剩测试代码级

## 2026-08-26（修复 — AI 聊天无回应：reasoning 模型输出字段兼容）

**动机**：用户报告 AI 聊天无论任何情况都无回应。根因：`coding` 模型（mimo-v2.5-free）将所有输出放在 `reasoning` 字段，而 `content` 始终为空。流式/非流式解析器只读取 `content`，导致完整响应被丢弃。

- **`ai/mod.rs`**：`StreamDelta` 新增 `reasoning: Option<String>` 字段，流式解析路径在 `content` 为空时回退到 `reasoning`；`ChatMsg` 同步新增 `reasoning` 字段，非流式路径（`chat()` + streaming fallback）同样回退

## 2026-08-25（修复 — Semgrep WARNING 清零：生产代码 unwrap/expect 消除）

**动机**：路线图 P0 最后一项。AGENTS.md 禁止 `unwrap()`/`expect()` 在非致命路径。

- **4 处生产代码 expect 修复**：
  - `webapi/mod.rs`：`format!("{bind}:{port}").parse().expect(...)` → match + log + graceful return（用户输入无效地址不再 panic）
  - `search/mod.rs`：`create_in_ram().expect(...)` → nosemgrep 标注修正（in-RAM 创建不可失败）
  - `search/searcher.rs`：`Regex::new(literal).expect(...)` → nosemgrep 标注修正（编译期验证的正则）
  - `extractor/paddleocr.rs`：`OnceLock::get().expect("just set")` → nosemgrep 标注修正（set 后必 Some）
- **根因**：3 处已有 `// nosemgrep: rust-unwrap` 标注但引用了不存在的规则 ID（实际规则为 `rust-expect-panic`）；1 处无标注且为真实 panic 风险
- **测试代码 494 处 unwrap/expect 保留**：AGENTS.md 允许测试路径使用

## 2026-08-25（功能 — AI 主题聚类 + sherpa-onnx 构建文档）

**动机**：路线图 P3「RAG 内容分析」（摘要✓、跨文件关联✓，主题聚类缺失）+ P0「sherpa-onnx-sys 构建依赖」。

- **新增 `ai_topic_clusters` IPC 命令**：取最近 N 个（默认 150，上限 400）活跃文档的 AI 摘要或内容前 200 字符，交 LLM 按 3-8 个主题分组，要求输出严格 JSON `[{topic, ids}]`；宽松解析（截取首 `[` 到末 `]`），编号映射回路径，空组/越界编号自动丢弃（`commands/ai.rs`）
- **索引状态页新增「主题聚类」卡片**：LLM 未配置时显示不可用文案；可用时显示提示 + 「✦ 开始聚类」按钮，结果按主题分组渲染（📁 主题 + 文件数 + 路径列表），失败红字可重试。3 个新 i18n 键 × 4 语言（`pages/IndexStatus.tsx`、`api/files.ts`、`i18n/{zh,en,ja,ko}.ts`）
- **lib.rs**：注册 `ai_topic_clusters`
- **sherpa-onnx 构建依赖（P0 勘误）**：核实上游 `sherpa-onnx-sys` build.rs 原生支持 `SHERPA_ONNX_ARCHIVE_DIR`（本地压缩包缓存）与 `SHERPA_ONNX_LIB_DIR`（已解压库目录），无需改代码——README 快速开始补充国内网络构建说明

**验证**：cargo test 202 lib 全绿、tsc 0、semgrep ERROR 0

## 2026-08-25（功能 — 长文档分块检索：RAG 注入不再截断，改选 top-K 相关段落）

**动机**：路线图 P3。RAG 注入每文件截断 50000 字符，281 个超长文档中的相关内容（如判决书中段的金额数字）被截掉。

**方案**（按 `.omo/plans/doc-chunking.md` 实施 Phase 1+2）：索引时分块存储，注入时按词法相关性选块。

**变更文件**：
- `db/chunks.rs`（新增）：`chunk_text`（~1500 字符块 + 200 重叠，句号/换行边界优先切分，>10000 字符才分块）；`replace_chunks`/`get_chunks`/`delete_chunks` CRUD；`select_relevant_chunks`（确定性分词：ASCII 词元整存 + CJK 二字滑窗，词频打分选 top-K 后按阅读序返回）；`run_backfill_chunks`（每次最多 500 文档，幂等收敛）。含 6 个单元测试（阈值/边界对齐/评分排序/CRUD 往返/回填幂等）
- `db/mod.rs`：新增 `doc_chunks` 表（**以 md5 为键**而非计划中的 file_id——内容按 md5 去重存储，同内容多路径共享一份块集，清理可挂接现有孤儿回收）；`cleanup_orphan_content` 扩展清理无引用的 doc_chunks 行
- `indexer.rs`：`extract_and_index_single` 提取完成后，长文档自动分块写入；去重复用路径跳过已存在块集；内容缩到阈值以下时清除过期块
- `commands/ai.rs`：新增 `chunked_or_truncated` 辅助——@mention 文件 >50K 字符且有块集时，注入 top-8 相关段落（带「第X-Y字」位置标记），否则回退截断；检索命中片段（2K）不变
- `commands/index.rs` + `lib.rs`：扫描完成后后台线程跑 `run_backfill_chunks`，存量长文档无需重建索引即受益

**验证**：cargo test 258 全绿（含 202 lib + 56 集成）、semgrep ERROR 0

## 2026-08-25（性能 — 启动扫描 walk 中分批提交：每 250 文件就地 flush）

**动机**：路线图 P1「启动扫描异步化」。三个扫描方法（full_scan/incremental_scan/startup_scan）都是先走完整个目录树再开始索引——大目录下走树期间零产出，且 jobs 全量驻留内存。

- **新增 `flush_jobs` 辅助方法**：封装 `batch_index` 调用 + 结果归集，返回 `(indexed_ok, errors)`。内部用 `std::mem::take` 清空 jobs，避免重复分配（`scanner/mod.rs`）
- **walk 内每 250 jobs 就地 flush**：三个扫描方法的遍历循环中，`jobs.len() >= FLUSH_EVERY` 时调 `flush_jobs`，与 `batch_index` 内部 CHUNK 对齐（250）。删除检测仍在走完树后进行，逻辑不变（`scanner/mod.rs`）
- **最终 flush 用 `flush_jobs` 替换**：三处 post-walk 的 `batch_index` 调用改为 `flush_jobs`，取消检查已内置于 helper 中，消除重复（`scanner/mod.rs`）
- **路线图勘误**：P1「IO 竞争缓解」经核实已实现（`DEFAULT_BATCH_IO_CONCURRENCY=8` + 专用限流 Rayon 池 + 并发峰值测试），从待办移除

## 2026-08-25（增强 — 扫描会话日志 RAII 化 + CLI 补齐）

**动机**：路线图 P2「索引会话日志」。4 处调用点重复相同的 SessionLog open/write/close 样板，且 CLI 路径（`link-searcher scan`/`watch`）完全缺失会话日志。

- **新增 `SessionLogGuard` RAII struct**：构造时打开日志（失败则惰性降级为 no-op），drop 时自动 flush + sync；暴露 `write_line()` 方法。消除 4 处重复的 open/write/close 样板（`logs/session.rs`）
- **4 处调用点迁移**：`trigger_scan`、`rebuild_index`（`commands/index.rs`）、`add_dir` 吸收后自动扫描（`commands/dirs.rs`）、启动扫描线程（`lib.rs`）全部改用 guard，错误提前 return 不再需要手动 close
- **CLI 补齐会话日志**：`link-searcher scan [dir]` 每目录写一条结果行；`watch` 基线扫描写开始/完成行。GUI 日志页现在能看到 CLI 触发的扫描记录（`cli.rs`）
- **跳过 progress 逐文件镜像**：scanner 的 `progress` 回调是 `impl Fn`（每文件触发一次），写入 session log 需 `&mut` 会破坏 Fn 约束，且 10 万文件会产生 10 万行——摘要行（开始/每目录结果/错误/完成）已足够
- **路线图勘误**：P1「浏览页动态分页」与「Tantivy reader 刷新」经核实均已实现（Browse.tsx ResizeObserver + IndexManager reader.reload() 1s 节流），从待办移除

## 2026-08-25（AI 推理追溯：append-only 事件日志）

**新增 `ai_events` 表**：每轮 AI 对话的 RAG 管道执行过程记录为结构化事件序列（query_rewrite → scope_resolved → retrieval → context_assembled → llm_call → turn_complete），持久化到 SQLite，前端可展开查看推理过程。

**变更文件**：
- `db/ai_events.rs`（新增）：事件 CRUD（record_event / get_session_events / get_turn_events / cleanup_old_events）+ 3 个单元测试
- `db/mod.rs`：新增 `ai_events` DDL（含 session+turn 复合索引）
- `commands/ai.rs`：`prepare_conversation_prompt` 在 4 个管道节点埋点（改写/scope/检索/组装），`conversation_ask_stream` 在 LLM 调用前后追加 llm_call + turn_complete 事件并批量写入 DB；新增 `get_ai_events` / `get_turn_ai_events` IPC 命令
- `lib.rs`：注册 2 个新 IPC 命令
- `components/AiEventTimeline.tsx`（新增）：可折叠推理过程时间线组件
- `components/ChatPanel.tsx`：每轮助手消息下方嵌入"🧠 推理过程"折叠面板
- `api/files.ts`：新增 `getAiEvents` / `getTurnAiEvents` API 函数
- `i18n/{zh,en,ja,ko}.ts`：新增 7 个多语言 key（ai_reasoning / ai_event_*）

---

## 2026-08-24（增强 — Web API 完整功能对等：65 个 REST 端点 + client.ts 全量映射 + SSE 事件总线）

**动机**：Web UI 和桌面 UI 功能完全一致——同一套 React 代码，改一处自动同步另一处。

**架构**：
- 后端拆分为按领域模块：`webapi/routes/{mod,search,files,index,dirs,config,settings,logs,backup,tesseract,ai}.rs`
- SSE 事件总线（`/api/events`）桥接 9 个 Tauri 事件到 web 客户端
- 前端 `listen()` 用 fetch + ReadableStream 解析 SSE 流

**新增 65 个端点**（总计 89 个命令全映射）：
- search: `GET /api/search/paths`、`GET /api/search/tree-prune`、搜索历史 CRUD、导出、统计
- files: `GET /api/files/browse`、`GET /api/dir-entries`
- index: 重建/批量重索引/重提取缺失/验证完整性/补齐语义向量/错误查询
- dirs: 增删改目录、目录树、子目录
- config: 配置 CRUD、Provider CRUD/刷新/测试、激活模型
- settings: 设置更新
- logs: 日志查看/清空/会话列表
- backup: 触发/状态/列表/导出/删除/死目录/路径重映射
- tesseract: OCR 引擎/依赖/文件类型支持
- AI: 摘要/智能搜索/流式/RAG 对话/会话保存/网关测试
- 桌面专属命令: 返回 501 Not Implemented

**client.ts 改造**：
- 替换 ~35 个占位映射为真实 HTTP 端点
- 添加 `paramMap` 参数名转换（camelCase → snake_case）
- 添加 `transform` 响应结构解包
- 实现 `listen()` SSE 支（fetch + ReadableStream 解析 SSE 事件流）

## 2026-08-23（新功能 — 前端统一：同一套 React 代码同时支持桌面和 Web）

**动机**：用户要求 Web UI 和桌面 UI 统一——改一处代码，两边自动同步。

**架构**：
- `src/utils/platform.ts` — 环境检测 `isTauri()` + 平台抽象（`confirm`/`alert`/`saveFile`/`exportFile`/`openDirectory`/`resolveAssetUrlSync`），Tauri 下走原生 dialog/fs，浏览器下走 `window.confirm`/Blob download
- `src/api/client.ts` — `client.invoke(command, args)` 自动路由到 Tauri IPC 或 HTTP fetch；`client.listen(event, handler)` 自动路由到 Tauri events 或 SSE
- 83 个 invoke 命令中 ~30 个已映射到 HTTP 端点，未映射的返回 null 并打 console.warn

**后端静态托管**：Web API 的 fallback 路由托管 Vite 构建产物（`dist/` 目录），SPA 路由回退到 `index.html`

**改造范围**：
- 8 个 api 文件：`invoke` → `client.invoke`
- 11 个 pages/components：dialog → platform 抽象、invoke → client.invoke
- PreviewPanel：`convertFileSrc` → `resolveAssetUrlSync`
- App.tsx MCP 插件用 `isTauri()` 条件包装

## 2026-08-23（新功能 — Web API 浏览器搜索页面）

- **浏览器访问 Web API**：`GET /` 返回自包含 HTML 搜索页面（零外部依赖、零构建步骤），含 Token 输入框（localStorage 持久化）、搜索框、结果列表（文件名/路径/大小/日期/相关度）、索引状态显示。页面不走 Bearer Token 认证层，API 调用由页面 JS 携带 Token 发起（`src-tauri/src/webapi/routes.rs`）

## 2026-08-23（新功能 — 安全的远程 Web API：HTTPS RESTful 端点 + Bearer Token 认证）

**动机**：路线图 P3 要求提供可选的远程 WebUI/API，让手机/平板/同事可通过 HTTPS 检索文档、预览文件、查看索引状态、触发扫描。默认关闭，需用户显式启用。

**技术选型**：`axum` + `axum-server`（rustls TLS）直接 spawn 于 `tauri::async_runtime`，非 `tauri-plugin-axum`（后者走 Wry 自定义协议，非真实 TCP，不支持 LAN 访问）。状态共享：clone `AppHandle` 注入 axum State，handler 内 `app_handle.state::<AppState>()` 访问全部 Tauri 管理的状态。优雅关停：`CancellationToken` + `axum_server::Handle::shutdown()` 在 `RunEvent::Exit` 触发。

**新增依赖**：`axum 0.8`、`axum-server 0.7`（tls-rustls）、`tower-http 0.6`、`tokio-util 0.7`、`rcgen 0.13`（自签名证书生成）

**新增文件**（`src-tauri/src/webapi/`）：
- `mod.rs` — 服务器启动/关停、Bearer Token 生成/加载、端口/绑定地址读取
- `state.rs` — `ApiState`（AppHandle + auth_token + cancel_token）
- `auth.rs` — Bearer Token 中间件（`axum::middleware::from_fn`）
- `tls.rs` — `rcgen` 自签名证书生成 + 持久化到 `{data_dir}/tls/`
- `routes.rs` — 20 个 RESTful 端点

**RESTful 端点**（全部需 `Authorization: Bearer {token}`）：
- 只读：`GET /api/search`、`GET /api/suggest`、`GET /api/files`、`GET /api/files/:id/preview`、`GET /api/index/status`、`GET /api/index/health`、`GET /api/dirs`、`GET /api/ai/capabilities`、`GET /api/version`、`GET /api/settings`
- 写入：`POST /api/scan/trigger`、`POST /api/scan/cancel`、`POST /api/reindex`、`POST /api/chat/ask`（非流式 RAG 问答）
- 会话：`GET/POST /api/chat/sessions`、`GET/DELETE /api/chat/sessions/:id`、`POST /api/chat/sessions/:id/export`

**安全边界**：
- 默认关闭（`web_api_enabled` 设置项，默认不存在=关闭）
- 默认绑定 `127.0.0.1`（仅本机），可选 `0.0.0.0`（局域网）
- 自签名 TLS 证书（首次启用时自动生成，持久化）
- 随机 32 字符 Bearer Token（`uuid::Uuid::new_v4().simple()`，持久化到 app_settings）
- 优雅关停：`CancellationToken` + `axum_server::Handle::shutdown()`

**设置页 UI**（`src/pages/Settings.tsx`，系统标签页新增 "Web API" 区块）：
- 启用/禁用开关
- 端口输入（默认 8443）
- Bearer Token 只读显示 + 重新生成按钮（`crypto.getRandomValues` 前端生成）
- 绑定地址下拉（仅本机/局域网）
- 启用时显示访问 URL + 重启提示

**改动文件**：`src-tauri/Cargo.toml`、`src-tauri/src/lib.rs`（spawn + RunEvent::Exit + `pub mod webapi`）、`src-tauri/src/commands/settings.rs`（白名单加 4 个 web_api_* key）、`src-tauri/src/commands/ai.rs`（`read_history`/`write_history`/`create_chat_session_impl`/`export_chat_session_impl`/`export_chat_session_json_impl`/`bm25_relevant_hits`/`truncate_text` 改 pub）、`src-tauri/src/db/dir_config.rs`（DirConfig 加 Serialize）、`src-tauri/src/commands/ai.rs`（ChatHistoryFile/ScoredHit 改 pub）、`src/pages/Settings.tsx`

## 2026-08-23（修复 — AI 聊天 Markdown 渲染：CJK 加粗不生效）

**根因**：CommonMark 的 emphasis flanking 规则将 CJK 字符视为"单词字符"（同英文字母），当 `**` 紧邻中文字符时（如 `有效建立了**"..."**的`），`**` 不满足左/右侧 flanking 条件，被当字面文本输出而非加粗标记。这是 CommonMark spec 层面的已知缺陷（[#650](https://github.com/commonmark/commonmark-spec/issues/650)，2020 年开至今未修）。

- **CJK 加粗渲染失败**：引入 `remark-cjk-friendly` 插件（官方 remark 插件目录收录，Vercel streamdown / Rspress / Cherry Studio 采用），在 micromark 层修复 CJK flanking 规则。对非 CJK 内容输出与 CommonMark 0.31.2 测试用例完全一致。改动 2 行：import + remarkPlugins 数组追加（`src/components/ChatPanel.tsx`、`package.json`）

## 2026-08-23（修复 — Web API 默认绑定地址改为局域网）

- **Web API 默认绑定 `127.0.0.1`**：改为默认绑定 `0.0.0.0`（局域网可访问），设置页选项顺序调整（`src-tauri/src/webapi/mod.rs`、`src/pages/Settings.tsx`）

## 2026-08-22（增强 — AI 聊天全链路追溯：日志 + 导出 + 会话存储）

**动机**：AI 聊天回答缺乏可追溯性——事后无法知道某轮回答使用了哪个模型、耗时多久、最终检索查询是什么、BM25 命中多少文档。`app.log` 和导出会话无法关联。

**方案**：在 `PerTurnEvidence`、`TurnExport`、`AiDone`、`ChatExportJson` 四个结构体同步增加 6 个追溯字段，`conversation_ask_stream` 注入 `[AI_TRACE]` 结构化日志，前端 `onDone` 回调持久化字段到会话存储。

- **新增字段 6 个**：`trace_id`（会话 ID#轮次，关联日志与导出）、`took_ms`（生成耗时）、`llm_model`（激活 LLM 模型 ID）、`embedding_model`（激活 Embedding 模型 ID）、`search_query`（改写后的最终检索查询）、`hits`（BM25 合并前命中数）。全部 `#[serde(default)]` — 旧会话无感兼容（`src-tauri/src/commands/ai.rs`）
- **Markdown 导出增强**：文件头部新增「追溯信息」块（LLM/Embedding 模型、语义权重、会话 ID、时间）；每轮问下新增 Trace ID、耗时、模型、最终检索查询、命中数元数据（`src-tauri/src/commands/ai.rs`）
- **JSON 导出升级**：`schema_version` 升至 2，`SessionExportMeta` 增加 `semantic_weight`/`llm_model`/`embedding_model`，`TurnExport` 增加 6 个追溯字段（`src-tauri/src/commands/ai.rs`）
- **结构化日志**：`conversation_ask_stream` 在检索开始和结束各打一条 `[AI_TRACE]` 日志，含 `trace_id`、模型、strict 模式、hits、search_q、took_ms、answer_chars、sources 等完整上下文（`src-tauri/src/commands/ai.rs`）
- **前端类型同步**：`PerTurnEvidence` 和 `AiDonePayload` 接口增加 6 个可选追溯字段（`src/api/files.ts`）
- **前端持久化**：`ChatPanel.tsx` 的 `onDone` 回调将 `trace_id`/`took_ms`/`llm_model`/`embedding_model`/`search_query`/`hits` 写入 `per_turn_evidence`（`src/components/ChatPanel.tsx`）

## 2026-08-22（修复 — AI 聊天检索管线 7 项缺陷：scope/语义/改写/去重/ASR/时间戳）

**根因**：导出会话 JSON 分析发现首轮路由走 `smartSearchStream` → `prepare_smart_prompt` 硬编码 `semantic:false`、`rewritten:false`、不传 `session_retrieval_scope`，导致 scope/语义/改写全部失效。`bm25_relevant_hits` 无 MD5 去重导致同内容不同路径文件重复命中。音频 ASR 参数不当导致转录乱码。

- **浏览页新增「导出识别文字」**：右键文件 → 导出提取/OCR 的文本到 .txt 文件。调 `preview_file` 获取完整文本（无截断），Tauri save 对话框选保存位置，`writeTextFile` 写入。空内容时提示。4 语种 i18n（`src/pages/Browse.tsx`、`src/i18n/{zh,en,ja,ko}.ts`）

- **首轮 scope 不生效**：`ChatPanel.tsx` `handleSend` 路由条件 `hasScope` 不检查 `session.retrieval_scope`，用户通过右键"加入检索范围"设了 scope 但首轮无 @mention 时走 `smartSearchStream`（不传 scope）。改为始终走 `conversationAskStream`（`prepare_conversation_prompt` 完整处理 scope 解析+语义+改写）（`src/components/ChatPanel.tsx`）
- **恢复挂起请求不走 scope**：`ChatPanel.tsx` resume effect `sourceIds.length === 0` 分支调 `smartSearch`（无 scope），改为始终调 `conversationAsk` 并传 `session.retrieval_scope` + `strict_docs`（`src/components/ChatPanel.tsx`）
- **语义搜索分数恒 null**：`prepare_smart_prompt` 硬编码 `semantic: false` + `semantic_score: None`。重构为调用 `bm25_relevant_hits`（内部按 `embedding_enabled()` 走 `semantic_fuse` 加权混合），`ScoredHit.semantic_score`/`rrf_score` 正确传播到 `EvidenceItem`（`src-tauri/src/commands/ai.rs`）
- **查询改写恒 false**：同上根因，`prepare_smart_prompt` 硬编码 `rewritten: false`。重构后走 `bm25_relevant_hits` 路径，one-shot 无历史时改写正确返回 false（无 parent 消息），多轮路径已在 `prepare_conversation_prompt` 中正确实现（`src-tauri/src/commands/ai.rs`）
- **搜索结果重复**：`bm25_relevant_hits` 收集 BM25 命中时不按 MD5 去重，同内容不同路径文件各占一条。加 `HashSet<String>` 跟踪已见 MD5，同 MD5 只保留首条（BM25 已按分降序，保留最高分）。`semantic_fuse` 在去重后的候选集上运行，自动受益（`src-tauri/src/commands/ai.rs`）
- **音频转录乱码**：FunASR-Nano 配置 `max_new_tokens: 512` 截断长段、`itn: 1` 数字乱码、硬切分无 overlap 切断词语、段间 `" "` 拼接破坏 CJK。改为 `max_new_tokens: 1024`、`itn: 0`、1s overlap 硬切分回退、段间 `""` 拼接（`src-tauri/src/extractor/audio.rs`）
- **会话 created_at=0**：前端创建 ChatSession 时 `created_at` 默认 0，`save_chat_session_impl` 的 `Some(existing)` 分支直接覆盖后端创建时的 `created_at: now`。改为 `created_at == 0` 时保留已有值（`Some` 分支）或设为 `now`（`None` 分支）（`src-tauri/src/commands/ai.rs`）
- **scope 过滤静默失效**：`searcher.rs` 的 `RegexQuery` 对含中文/全角字符的 STRING 字段路径匹配可能静默失败，fallback 到 `AllQuery` 禁用过滤。在 `bm25_relevant_hits` 加 post-filter 安全网：BM25 结果按 `path_prefixes` 再过滤一次，保证 scope 正确性。同时增大 fetch 量补偿 post-filter 损失（`src-tauri/src/commands/ai.rs`）
- **scope 文件路径不解析为 file_id**：`session_retrieval_scope` 中的文件路径只进 `path_prefixes`（RegexQuery + post-filter），但 BM25 top-50 命中里可能没有该文件 → post-filter 后为空 → strict_docs 拒绝回答。改为先 `get_file_by_path` 精确匹配 + `search_file_ids_by_path_fragment` LIKE 回退解析为 `file_id`，用可靠的 TermQuery 而非 RegexQuery（`src-tauri/src/commands/ai.rs`）
- **strict_docs 默认 false**：用户期望引用文件时默认严格模式。`create_chat_session_impl` 和前端 `AiChat.tsx` 两处创建 ChatSession 改为 `strict_docs: true`（`src-tauri/src/commands/ai.rs`、`src/pages/AiChat.tsx`）
- **per_turn_scopes 导出为空**：`patchSession` 用闭包中的陈旧 `session` 做 spread，catch 分支（错误回答）覆盖了 step 1 设的 `per_turn_scopes`。改为 `patchSession` 优先用 `sessionRef.current`（最新值），保留 async 调用间累积的 `per_turn_scopes`（`src/components/ChatPanel.tsx`）
- **semantic_fuse 诊断日志**：cosine 全零时打 WARN 显示 query 向量维度/非零状态/doc 维度，便于排查 embedding 模型问题（`src-tauri/src/commands/ai.rs`）
- **scope 内文件搜不到**：`session_retrieval_scope` 文件路径先 `get_file_by_path` 精确匹配 + `search_file_ids_by_path_fragment` LIKE 回退解析为 `file_id`，用 TermQuery 而非 RegexQuery；BM25 零命中时用空查询重试（match-all within file_ids scope），确保 scope 内文件内容被返回即使查询词不匹配索引内容（`src-tauri/src/commands/ai.rs`）
- **检索范围可重复添加**：`mergeScopePrefixes` 只做父吞子去重，不去精确重复。加 `new Set()` 去重后再父吞子（`src/utils/scopeMerge.ts`）
- **引用文件行为不统一**：`retrieval_scope` 文件只做 BM25 过滤（可能零命中→strict_docs 拒绝），而 @mention 文件直接注入 LLM prompt。两者行为不一致，且 @mention 是单轮 chip（发送后清空），与会话级设计冲突。改为 `retrieval_scope` 文件解析后也直接注入（带 [N] 编号），与 @mention 统一行为，每轮都注入直到用户删除（`src-tauri/src/commands/ai.rs`）
- **扫描件 PDF 水印检测漏判导致 OCR 不触发**：`try_pdftotext_extract` 的 `is_watermark_text` 过滤条件 `n.len() > 20`（字节）对短中文水印（如"证据第页"=12字节）过滤过严，导致 normalize 后的页面全部被过滤掉→`normalized` 为空→水印检测返回 false→pdftotext 返回的 640 字符页码文本被误当有效内容→OCR 从未运行。改为 `n.chars().count() > 2`（字符数），让短中文水印能被检测（`src-tauri/src/extractor/pdf.rs`）

## 2026-08-21（修复 — 文件树搜索 JSX 解析错误 + 树剪枝搜索）

**根因**：`AiChat.tsx` 文件树搜索块用三层嵌套三元 + 括号化 JSX 分支写在单个 `{}` 内，`oxc`/esbuild 解析器把第 488 行 `)` 误判为外层分组关闭、导致 502 行 `:` 无匹配 `?`。此前 3 个 commit（`efac8f4`/`c119904`/`2698543`）均只重新平衡括号，未消除嵌套三元本身这个解析器陷阱，故每次 `tauri dev` 重新报错。

- **文件树搜索 JSX 反复编译失败**：将三层嵌套三元改为 IIFE（`(() => { if … return … })()`），零嵌套三元、零行为变化，`oxc` 干净通过（`src/pages/AiChat.tsx`）
- **dirs.rs 未使用变量警告**：`build_dir_tree` 中 `let (indexed, status)` 的 `indexed` 从未读取（line 357 已内联重算），改为 `let (_, status)` 消除 `unused_variables` 警告（`src-tauri/src/commands/dirs.rs`）
- **文件树搜索改为树剪枝算法**：`AiChat.tsx` 树过滤框从 `searchFilePaths`（SQL `LIKE` 返回所有文件路径）改为 `search_tree_prune`（Rust 端逐段检查路径，每分支取最浅命中节点——目录或文件，命中即停不向下展开，祖先吞并后代）。全量返回无上限。前端加最短输入拦截（半角 ≥3 / 全角 ≥2）（`src-tauri/src/commands/search.rs`、`src-tauri/src/lib.rs`、`src/api/files.ts`、`src/pages/AiChat.tsx`、`src/i18n/{zh,en,ja,ko}.ts`）

## 2026-08-21（内置 BGE 嵌入模型 — 本地离线语义搜索）

**动机**：支持无需远程 API 的本地语义搜索，隐私优先，离线可用。

- **备份管理增强**：导出历史快照支持（`export_backup` 新增 `backup_name` 参数，可导出任意历史快照为 zip，支持 AES-256 加密）；Settings 备份 Tab 新增「备份列表」Section（调 `listBackups()` 展示全部快照，每项带「导出此快照」+「恢复」按钮，恢复前二次确认；底部注明「自动保留最近 10 个快照，超出时合并压缩」）。恢复双路径（从内部备份列表直接恢复 / 从外部 ZIP 恢复）。导出对话框改用 `save()` 支持自定义文件名，默认 `link-searcher-backup-YYYY-MM-DD.zip`。4 语种 i18n 补全（`src-tauri/src/commands/backup.rs`、`src/pages/Settings.tsx`、`src/api/backup.ts`、`src/i18n/{zh,en,ja,ko}.ts`）

- **新增依赖**：`tract-onnx = "0.20"`（ONNX 推理，与 PaddleOCR 共用 tract 引擎）、`tokenizers = "0.20"`（HuggingFace tokenizer）（`src-tauri/Cargo.toml`）
- **本地嵌入引擎**：新建 `ai/local_embed.rs`，基于 tract-onnx + tokenizers 运行 BAAI/bge-small-zh-v1.5（95MB ONNX + 5MB tokenizer.json，512 维，中英双语）。OnceLock 单例模式，CLS pooling + L2 归一化。BGE 查询指令前缀硬编码（`src-tauri/src/ai/local_embed.rs`）
- **BGE 模型下载命令**：新建 `commands/bge.rs`，`install_bge` 从 hf_mirror/ModelScope 首次使用时下载到 `data_dir/models/bge-small-zh-v1.5/`，`check_bge_installed` 检查模型就绪状态。AtomicBool 防重入，后台线程 + 事件通知（`src-tauri/src/commands/bge.rs`）
- **嵌入管线双模式分叉**：`ai::embed_batch()` 和 `ai::embed()` 检测 `active_embedding_model_id` 是否以 `"local:"` 开头，是则走本地 ONNX 推理（query 模式加指令前缀），否则走现有 HTTP 路径。`embedding_enabled()` 同时检测本地模型文件就绪状态。所有下游代码（搜索/回填/聊天）零修改（`src-tauri/src/ai/mod.rs`）
- **配置格式扩展**：`set_active_model` 支持 `"local:bge-small-zh-v1.5"` 格式，跳过 provider 查找。新增 `is_local_embedding_model()` 辅助函数（`src-tauri/src/config.rs`、`src-tauri/src/commands/config.rs`）
- **Settings UI 集成**：Settings 页 AI 标签 Embedding 下拉菜单顶部新增「内置 / bge-small-zh-v1.5」选项（已安装时），未安装时显示下载按钮。4 语种 i18n 补全（`src/pages/Settings.tsx`、`src/api/settings.ts`、`src/i18n/{zh,en,ja,ko}.ts`）

## 2026-08-20（备份功能 Phase 0：依赖、seed 死键修复、公共快照引擎、前端封装）

**动机**：跨平台备份/恢复功能开工（设计见 `.omo/plans/backup-cross-platform.md`）。Phase 0 打地基：依赖就绪、修历史遗留 seed 死键、抽公共快照引擎供后续增量链/导出/恢复复用。

- **新增 Cargo 依赖**：`zip` 补 `aes-crypto` feature（导出加密用）、新增 `sha2`（清单校验和）（`src-tauri/Cargo.toml`）
- **修复 app_settings seed 死键**：seed 写 `auto_backup_enabled`/`auto_backup_interval_days` 但 UI/白名单读 `auto_backup`/`backup_interval`，导致"自动备份"开关默认失效、设置永不生效。修改 seed 为新键名 + 新增 `migrate_legacy_backup_settings` 幂等迁移（旧值复制到新键、删除死键），带迁移测试（`src-tauri/src/db/mod.rs`）
- **新增公共快照引擎** `snapshot_core`：快照 `.ls-index` + `data.db`（SQLite 在线备份 API，WAL 安全）+ `config.json`（config_dir）+ `chat_history.json`（data_dir），返回含 sha256 的 `SnapshotManifest`；`trigger_backup` 改走引擎并写 `snapshot.json`；新增集成级单测（`src-tauri/src/commands/backup.rs`）
- **新增前端 IPC 封装** `src/api/backup.ts`：9 个命令的类型化包装（触发/状态/列表/导出/恢复/目录重映射）（`src/api/backup.ts`）
- **段级增量备份链**：`trigger_backup` 从"整目录全量复制"改为增量——Tantivy 不可变 segment 首次复制后，后续快照经 `.chain.json`（atomic tmp+rename）记录的 `segment_store` 直接硬链接复用（跨设备失败回退复制）；`meta.json`/`.managed.json` 等原子替换小文件与 `data.db`/`config.json`/`chat_history.json` 每次全量（体积大头在 segment，DB 为 MB 级不值得页级增量）。`get_backup_status` 改从链计算物理占用（每快照小文件一份 + 每个 segment 全局一份）。链文件损坏自动重置为空链而非阻断备份（`src-tauri/src/commands/backup.rs`）
- **备份链合并压缩**：新增 `merge_chain(N=10)`，当快照数达到阈值时合并最旧 N-1 个为 consolidated "merged" 快照（segment 硬链接复用，非 segment 文件取最新版），旧目录删除并更新 baseline。新增 `prune_orphan_dirs` 清理未引用目录。`trigger_backup` 用 `merge_chain` + `prune_orphan_dirs` 替换原来简单 LRU 裁剪 + `cleanup_old_backups`。带合并集成测试（`src-tauri/src/commands/backup.rs`）
- **新增 list_backups IPC 命令**：读取 `.chain.json` 返回 `BackupSnapshot[]`（id/ts/kind/size），注册到 `lib.rs` invoke_handler（`src-tauri/src/commands/backup.rs`、`src-tauri/src/lib.rs`）
- **新增 export_backup 导出 zip 命令**：`snapshot_core` 快照当前状态到临时目录 → 打包为单 zip（Deflated 压缩），可选 AES-256 加密（`password` 参数非空时启用）。自动检测 config 中 API Key 等秘密并返回 `has_secrets` 标志。注册到 `lib.rs` invoke_handler（`src-tauri/src/commands/backup.rs`、`src-tauri/src/lib.rs`）
- **新增 restore_from_zip 命令**：从 zip 恢复备份（支持 AES-256 解密密码）。提取到临时目录，验证 `snapshot.json` 清单，复用 `restore_from_dir` 公共恢复引擎（索引切换 + 数据库在线备份恢复）。提取 `restore_from_dir` 公共函数，`restore_backup` 同步迁移复用。注册到 `lib.rs` invoke_handler（`src-tauri/src/commands/backup.rs`、`src-tauri/src/lib.rs`）
- **新增死目录处理命令**：`get_dead_dirs` 列出磁盘上不存在的监控目录（含关联文件数），`remap_dir` 更新目录路径（file_tracking 用相对路径无需迁移），`remove_dir_with_files` 删除目录配置及所有关联文件记录。新增 `count_files_by_dir` / `delete_files_by_dir` DB 辅助函数（`src-tauri/src/db/tracker.rs`、`src-tauri/src/commands/backup.rs`、`src-tauri/src/lib.rs`）
- **新增备份设置页 Tab**：设置页新增「备份」标签页，含备份触发/状态展示、ZIP 导出（可选密码 + has_secrets 风险提示）、ZIP 恢复（文件选择器）、死目录列表与重映射/删除操作。4 语种 i18n 键补全（`src/pages/Settings.tsx`、`src/i18n/{zh,en,ja,ko}.ts`）
- **备份功能测试补全**：新增 `test_has_config_secrets`（API Key 检测逻辑覆盖 legacy/embedding/LLM/provider 四类密钥）和 `test_snapshot_core_creates_zip_ready_dir`（验证快照产物目录结构完整且清单 size 一致）（`src-tauri/src/commands/backup.rs`）
- **备份功能文档补全**：README.md 索引与监控表新增增量备份链/ZIP 导出恢复/死目录重映射描述；`docs/09-backup-migrate.md` 补充增量备份链机制、ZIP 导出（AES-256 加密）、ZIP 恢复、死目录处理四节，验证清单同步扩展
- **StatusBar 备份状态**：状态栏右下角展示上次备份时间（每 60s 刷新），4 语种 i18n 补全（`src/components/StatusBar.tsx`、`src/i18n/{zh,en,ja,ko}.ts`）

---

## 2026-08-20（修复：Windows/Linux 构建失败——OCR 平台 stub 缺失 + opener 错误类型）

**动机**：v0.1.4 构建中 Windows/Linux 平台编译不过。

- **E0425 `recognize_from_path_with_regions` 未找到（Windows/Linux）**：`apple_vision.rs` 有非 macOS 的 `recognize_from_path` stub 但漏了 `recognize_from_path_with_regions`，`ocr_image_with_regions` 的 AppleVision 分支在非 macOS 编译失败。修复：补上非 macOS stub（与 `windows_ocr.rs` 模式一致）
- **E0308 `opener::open` 错误类型不匹配（Linux）**：`reveal_in_file_manager` 声明返回 `io::Result` 但 `opener::open` 返回 `Result<(), OpenError>`。修复：`map_err` 转为 `io::Error::other`

---

## 2026-08-20（修复：Windows Release 构建失败——build.rs 使用 Unix 专属 API）

**动机**：v0.1.3 构建中 Windows 平台失败——`build.rs` 编译不过。

- **根因**：`build.rs` 无条件 `use std::os::unix::fs::PermissionsExt`，并在复制 poppler/ffmpeg 后调用 `Permissions::from_mode(0o755)`（Unix 专属）；Windows 上该 API 不存在（复制循环因 macOS 路径不存在本来就不会执行）
- **修复**：`src-tauri/build.rs` 给 import 和两处 `set_permissions` 加 `#[cfg(unix)]`

---

## 2026-08-20（修复：Linux Release 构建缺少 pipewire 系统库）

**动机**：v0.1.2 构建中 Linux 平台失败——`libspa-sys` build script 找不到 `libpipewire-0.3`。

- **根因**：`tauri-plugin-mcp`（debug-only 插件，但依赖无条件声明）→ `xcap` → `pipewire` → `libspa-sys`，Release 构建同样编译该链，而 CI 未安装系统库
- **修复**：`.github/workflows/release.yml` Linux 依赖步骤补装 `libpipewire-0.3-dev` + `libspa-0.2-dev`
- **备注**：插件在 release 中不初始化，仅依赖被编译；后续可作为非默认 feature 门控以彻底移除编译开销

---

## 2026-08-20（修复：GitHub Actions 四平台 Release 构建失败）

**动机**：打 tag 触发 Release workflow，aarch64/x86_64 macOS、Windows、Linux 全部构建失败。

- **E0583 `logs` 模块缺失（全平台）**：`.gitignore` 裸 `logs` 规则匹配任意层级的 `logs` 目录，把 `src-tauri/src/logs/`（mod.rs + session.rs）整个忽略、从未提交；CI 检出后 `pub mod logs;` 找不到文件。修复：删除该 ignore 条目（运行时日志写入 OS 数据目录，不进仓库），并强制提交 `logs` 模块
- **objc2 编译失败（Windows/Linux）**：`objc2`/`objc2-foundation`/`objc2-vision` 是无条件依赖但仅 macOS 可用（Apple Vision OCR）。修复：移入 `[target.'cfg(target_os = "macos")'.dependencies]`
- **ort-sys 无 x86_64 macOS 预编译产物**：`ort = "2.0.0-rc.13"` 是全库无引用的死依赖（PaddleOCR 走 tract、embedding 走 HTTP 网关、音频走 sherpa-onnx）。修复：从 `Cargo.toml` 删除

---

## 2026-08-18（文档：AI 聊天检索范围章节重写）

**动机**：第八章「控制检索范围」仍描述旧交互（专注模式/范围按钮），与统一后的 `retrieval_scope` 设计不符。

- 重写 `docs/08-ai-features.md` 检索范围章节：统一范围概念（跨轮累计）、文件树右键加入（文件/目录统一）、范围条 × 删除、父目录吞并子路径（A∪A/B=A）、`/范围:` 命令、跨轮累计与每轮范围快照导出
- 同步「左侧面板」文件树描述（移除专注分析残留）
- 保留：@引用、/ext /date 条件命令、仅依据文档、编号引用

---

## 2026-08-18（浏览/搜索合一：Browse 嵌入全文搜索模式）

**动机**：搜索（SearchPage, 306 行）和浏览（Browse, 716 行）是独立路由，但用户高频在两者间切换查找文件——Browse 已有文件名过滤，缺全文搜索能力。

**合一方案**（渐进式）
- Browse 顶部工具栏嵌入 `<SearchBar>`（复用 SearchPage 的 SearchBar 组件），有查询时整个页面切换为搜索模式：
  - 表格区域替换为 `<ResultList>`（搜索结果，含高亮——Tantivy `<em>` → `<mark>` 渲染，搜索高亮已存在）
  - 选中结果 → 同步到右侧预览面板（复用 Browse 现有 `selectFile` + 内联预览）
  - 分页 / 结果计数 / 错误 / 空态 / 加载态全部覆盖
- 无查询时保持标准浏览表（文件名过滤、状态筛选、列宽拖拽、AI Q&A 不变）
- 搜索模式使用 `useSearch` hook 独立实例，与 SearchPage 共享同一搜索后端
- 路由不动：`/` → SearchPage（保留），`/browse` → Browse（合一后入口）
- 搜索结果高亮已由 ResultList 在 2026-08-16 的功能中实现，本次零改动

**验证**：cargo test 161 绿、tsc 0、semgrep 0

---

## 2026-08-18（前端检索范围统一 + P6 残留收尾）

**动机**：后端已收敛为单一 `retrieval_scope`（跨轮累计路径条目），但前端仍残留旧概念：专注（focus_file）、会话目录范围（scope_dir_ids）、ScopeEntry 条目、P6 私密按钮/提示——4 个入口并存，与"一个入口（右键）+ 一个范围"的设计相悖。

**前端统一入口**
- `AiChat.tsx` 文件树右键统一为「加入检索范围」：文件/目录右击都写入 `retrieval_scope`（原：文件右击=专注分析、目录右击=加入对话）；删除专注模式（`handleFocusFile`/`focus_file`）与 `handleScopeAction` 的 dir_id 解析（改存路径）
- 树根目录「范围」按钮改为切换 `dt.basePath` 进范围（原 scope_dir_ids → 绝对路径）
- `ChatPanel.tsx` 范围条统一为 `retrieval_scope` 条目 chips（📁/📄 按扩展名判别），逐条 × 删除；删除专注 chip/范围目录 chips/私密过滤提示三个旧区块
- `chat ask` 参数改为 `sessionRetrievalScope`：`conversationAsk`/`conversationAskStream` 签名对齐后端 `session_retrieval_scope`
- 每轮发送：chips 引用并入 `retrieval_scope`（前端 `mergeScopePrefixes` 父吞子去冗余，与后端语义一致），per-turn 快照写 `{turn_index, scope}`（align 后端 `PerTurnScope.scope`）
- 恢复挂起请求路径同步传 `retrieval_scope`

**类型清理**
- `files.ts`：删除 `ScopeEntry` 接口与 `ChatSession.scope_dir_ids/scope_entries/scope_conditions/focus_file` 字段；`PerTurnScope` 改 `scope: string[]`
- `DirManager.tsx`：删除私密标记按钮（`handleTogglePrivate` + 私有按钮 UI，P6 移除收尾）
- i18n 4 语言：删除 `private_dir`/`private_dir_on`/`private_dir_off`/`public_dir`/`scope_private_filtered`/`focus_mode`/`scope_dir`/`focus_overridden` 残留键

**后端 & DB**
- `prepare_conversation_prompt`：`retrieval_scope` 解析对齐 `@目录`——监控根绝对路径→`dir_ids`，相对子路径→`path_prefixes`（原先全塞 prefix 导致绝对路径匹配失效）
- `db/mod.rs`：新增 `drop_dir_config_private_column` 迁移（旧库 `dir_config.private` 列幂等 DROP），SCHEMA_VERSION → 3，含模拟旧库测试

**验证**：cargo test 全绿（lib 161 + 集成全过）、tsc 0、semgrep ERROR 0、旧字段引用全库 0 残留

- **README 同步**：检索范围控制功能描述更新为统一右键入口（原「专注分析/加入对话/设为会话范围」旧交互）

---

## 2026-08-18（修复 P6 私密目录移除后的测试连锁崩溃）

**根因**：删除私密目录功能时两处 SQL 残留未清理，导致所有 DB 依赖测试建表/插入即失败（44 个测试挂）：
1. `db/mod.rs:169` `CREATE TABLE dir_config` 中 `updated_at INTEGER NOT NULL,` 尾逗号悬空（原 `private` 列删除后未去掉）→ SQL 语法错误 `near ")": syntax error`，迁移执行失败 → 44 个测试建表即炸
2. `db/dir_config.rs:54` `INSERT INTO dir_config ... VALUES (?1..?8, ?8, 0)` 残留 `, 0`（原 `private=0` 硬编码）→ 10 值对 9 列 → `add_dir` 全部失败

scanner/index/search 测试并非逻辑错误，而是建表失败后的连锁崩溃。修掉两处残留后 **214 个测试全绿**（lib 160 + 集成 9+15+4+3+9+6+4+2+2）。

---

## 2026-08-18（会话导出：新增 JSON 格式 + Markdown 空范围标注）

**动机**：用户用导出文件分析 AI 问答行为，但 Markdown 缺少结构化字段（分数/旧来源标记），且空检索范围轮次被静默省略——"第一轮没指定范围"在导出中不可见。

- **新增 `export_chat_session_json` command**：分析友好 JSON——每轮含 `scope`（空数组=未指定全库）、`question`、`answer`（null=无回答）、完整 `evidence` 原始字段（file_id/path/snippet/bm25_score/semantic_score/rrf_score/rewritten/rewritten_query/from_history），会话级含 `retrieval_scope`/`strict_docs`（`src-tauri/src/commands/ai.rs`，lib.rs 注册）
- **Markdown 空范围标注**：每轮快照 scope 为空时渲染「未指定（全库）」，不再省略；无快照数据的旧会话保持不渲染（不臆断）
- **前端**：导出按钮保存对话框提供 JSON/Markdown 两种过滤器，按所选后缀调用对应导出（`src/api/files.ts`、`src/pages/AiChat.tsx`）
- **测试**：`export_json_marks_unset_scope_and_full_evidence`（空 scope 轮次 `[]`/answer null/依据字段）、`export_markdown_shows_unset_scope`（每轮空范围标注 + 顶部空会话不显示）

**验证**：cargo test 170 绿（lib 170 + 集成 9 + 4）、tsc 0

---

## 2026-08-18（检索范围验收清单 + 前端父吞子修复 + 后端 strict/mention 修复）

**动机**：折腾几天没达标，根因是"要求"从未写成可验证清单。先固化验收标准，再修实现，最后逐项对照验收。

**验收清单**
- 新增 `.omo/acceptance/retrieval-scope.md`：10 项 Given/When/Then 验收标准（A1-A10），每项含验证方式（cargo/vitest/手动）

**前端父目录吞并子路径修复（Bug A）**
- 新建 `src/utils/scopeMerge.ts`：`mergeScopePrefixes` 纯函数，加入即合并（父吞子 + 空串吸收）
- `AiChat.tsx`：`handleAddToScope` 使用合并逻辑；`/范围:` 命令统一路径格式（全库→`""`、子目录→相对路径）
- `ChatPanel.tsx`：范围条渲染合并后结果；`effectiveScope` 用合并后结果；`""` 条目渲染为"全库"；`×` 全库条目→清空
- 激活 vitest 运行器 + 13 个 scopeMerge 单测全部通过

**后端严格模式与引用文件依据修复（Bug B）**
- 新增 `resolve_mention_file_ids` 辅助函数：精确匹配→LIKE 回退→歧义检测（0/≥2 候选→missing）
- 搜索限制路径与证据直接使用路径统一使用同一解析器
- `[N]` 编号仅覆盖解析成功的 mention，missing 项排除
- strict_docs 强化：缺失/歧义/未索引→对应错误文案
- 严格模式下旧来源循环不执行（from_history 排除）
- 导出：空串条目显示为"全库"
- 新增 7 个集成测试全绿（含 zero-overlap、missing、ambiguous、history-exclusion、dir-no-false-error、empty-scope、unset-scope）

**验证**：cargo test 168 绿、npm test 13/13 绿、tsc 0

---

## 2026-08-16（检索范围重新设计：并集合并 + 统一存储 + 跨轮累计）

**动机**：原有四个概念（检索范围/引用/专注/仅依据文档）相互重叠，且"范围"与"子目录"是 AND 交集——设 A 目录 + A/B 子目录实际只搜 A/B（反直觉）。重新设计为一个正交概念（检索范围）+ 一条回答策略（仅依据文档）。

**概念收敛**
- **检索范围**＝跨轮累计的文件/目录集合（用户主动声明即加入，跨轮生效直到手动×删除）
- **引用**（@ 文件/目录）并入范围，不再"发完即走"
- **专注**并入范围（单文件条目）
- 仅依据文档保留为独立回答策略开关（不参与范围）

**后端并集合并（bug 修复）**
- 新增 `merge_scope_prefixes()` 纯函数：父目录路径吞噬其下的子前缀，消除 AND 交叉（`A∪A/B=A`）
- 接入 `prepare_conversation_prompt`：`dir_ids`（监控根）+ `path_prefixes`（子目录前缀）统一去冗余后 OR 检索
- 关联测试 4 个（父吞子/跨根保留/嵌套去最短/无 dir 时保留）——`src-tauri/tests/merge_scope_test.rs`

**数据模型**
- 新增 `ScopeEntry { kind: "dir"|"file", value: String }` 统一范围条目
- `ChatSession.scope_entries`（`#[serde(default)]` 兼容旧会话），`scope_dir_ids` 保留兼容
- IPC 命令 `conversation_ask`/`conversation_ask_stream` 加 `session_scope_entries` 参数（前后端同步）

**前端累计范围**
- `handleSend` 不再清空 `mentionChips`，而是把当前引用 chips 去重并入 `session.scope_entries`（跨轮累计，持久化）
- 约束面板显示 `scope_entries` 条目（📁 目录/📄 文件，逐条可×删除）
- TS 类型：新增 `ScopeEntry` 接口 + `ChatSession.scope_entries` 字段

**验证**
- 全量 `cargo test` 绿、`tsc -b` 0、scopeParser + translateErr 断言全过、semgrep ERROR 0

---

## 2026-08-16（检索范围可视化 · 操作同时更新）

**动机**：约束面板已静态显示会话级约束，但三类"操作"无实时可视化——`/范围:` 命令打字零预览、直接键入的 `@路径` 生效却无反馈、无合并生效范围。致"面板显示≠实际检索"。

- **`/范围:` 打字预览**：`handleInputChange` 复原 `parseScope` 的 `scopeAction`（原被丢弃），输入 `/范围:xxx` 即时显示紫色预览行「范围预览: 切换范围至目录 xxx」、`/范围:全库` 显示「切换为全库检索」；发送/切会话自动清空（`src/components/ChatPanel.tsx`）
- **合并生效范围摘要**：新增 `effectiveScope` useMemo——发送前实时显示"本轮将搜"：chips 引用（覆盖专注）/ 📌 专注文件 / 📁 范围目录 / /命令条件 / 「已排除 N 个私密目录」五类合并，只读摘要行（`本轮检索范围: ...`）
- **专注模式"说谎"修复**：`mentionChips.length > 0`（本轮有显式引用）时 focus 实际被跳过，原面板仍显 amber 📌。现显示灰色斜体待机态「📌 专注: X（已被引用覆盖，本轮不生效）」
- **i18n**：新增 `scope_preview`/`scope_preview_all`/`scope_preview_dir`/`effective_scope`/`focus_overridden` 键（4 语言）
- **单测**：scopeParser 增 S14/S15（/范围 → scopeAction `dir:xxx`/`clear` + cleanText 剥离）锁定打字预览依赖的解析
- **验证**：`tsc -b` 0、scopeParser + translateErr 断言全过、`cargo test --lib ai::` 32 绿、semgrep ERROR 0
- **说明**：MCP 交互实测受 tauri-dev 事件监听线程不稳定影响（非业务代码），可视化链路以纯函数单测 + 代码审查兜底验证

---

## 2026-08-16（检索范围约束面板 · 约束可视化）

**动机**：AI 聊天有 10 种检索范围约束（专注/范围目录/严格模式/P6 私密/命令条件等），此前只有专注 chip 与严格 toggle 两处分散显示，会话级约束不可见——用户不知道检索被缩到哪，易误判"AI 答错"。

- **统一约束条**：ChatPanel 输入框上方升级为完整约束面板，显示当前会话全部生效约束：
  - **范围目录 chips**：`session.scope_dir_ids` 逐个显示（dirScopes 映射出目录 label，找不到回退 id 前缀），带 × 可清除（`src/pages/AiChat.tsx`、`src/components/ChatPanel.tsx`）
  - **P6 私密过滤提示**：无显式目录范围且存在私密目录时显示「已排除 N 个私密目录」（设置页控制，纯提示不可清除）
  - **空态**：专注/范围/私密均未生效 → 「未限制（全库）」
  - 保留原有专注 chip（📌 amber）与严格模式 toggle（green），样式沿用现有体系
- **数据链路**：AiChat 的 `dirTrees` 补存 `private` 字段（原来自 listDirs 后丢弃），整树作为 `dirScopes` prop 传给 ChatPanel——零后端改动
- **i18n**：新增 `scope_range`/`scope_dir`/`scope_private_filtered`/`clear_scope`/`no_scope` 键（4 语言）
- **验证**：`tsc -b` 0、scopeParser + translateErr 断言全过、semgrep ERROR 0

---

## 2026-08-16（AI 聊天会话管理优化 + 批量多选/删除/导出 + 空响应修复）

### 会话批量管理

- **批量多选**：侧栏头部 ☑ 按钮进入批量模式（每项显示 ✔ 复选框），或 **Cmd/Ctrl+点击 直接多选**（免切模式，与浏览页交互一致）
- **批量删除**：选中多项后点删除 → `ask` 确认（显示数量）→ 逐项删除 → 若含当前会话则重置 active
- **批量导出**：选中多项后点导出 → 各会话 Markdown 合并为一个文件（`ai-chats-batch.md`）
- **批量操作栏**：selectMode 下显示「已选 N 项 / 全选 / 导出 / 删除 / 完成」，全选作用于当前过滤结果

### 会话列表优化

- **搜索框**：按标题实时过滤（本地过滤，零 IPC 改动），空结果显示空态
- **时间筛选**：全部 / 今日 / 7天 / 更早 四档
- **时间显示**：今日→HH:mm，之前→MM/DD
- **标题 hover 全文提示** + **侧栏拖拽调宽**（160-480px，localStorage 持久化）
- **移除 `@第N轮`/`@上轮` 死代码**：parser 残留造成"输入被剥离零效果"的假功能，现移除（后端字段保留 serde 兼容）
- **i18n**：新增 `batch_manage`/`selected_count`/`select_all`/`confirm_delete_sessions`/`search_sessions`/`session_range_*` 等键（4 语言）

### AI 空响应显式报错

- 网关偶发返回空 SSE 流（`content_chars=0`）时，ChatPanel 三处响应路径显示「❌ AI 未返回任何内容，请重试」（`err_empty_response`），不再静默空消息
- 新增 wire 测试 `chat_stream_empty_done_returns_empty_string` 锁定后端边界
- 真机验证：用户原句实测 41s 空流 → 错误提示；重试成功

### 质量门

- `cargo test` 全绿、`tsc -b` 0、scopeParser 单测全绿、semgrep ERROR 0
- 说明：MCP 实测受 tauri-dev 事件监听线程不稳定（`listener.rs:174` panic，非业务代码）影响，部分交互用代码验证兜底

---

## 2026-08-16（AI 空响应显式报错 · 根因记录）

### AI 空响应显式报错

- **根因**：网关（9router coding）对长思考统计类查询偶发返回空 SSE 流（`content_chars=0`），前端 `ai-done` 静默接受空文本 → 用户看到"只有 ⏱ 没有回答"
- **修复**：`ChatPanel` 三处响应路径（流式 done / smart_search 回退 / conversation_ask 回退）在 `full_text`/`answer` 为空时显示 `❌ AI 未返回任何内容，请重试`（i18n `err_empty_response`），替代静默空消息
- **新测试**：`ai_chat_stream_wire.rs` 增 `chat_stream_empty_done_returns_empty_string`（空 SSE 流 → `Some("")` 非 None，锁定后端边界）
- **真机验证**：用户原句"分年度统计陈骥代理的案件数量"实测 41s 空流 → 显示错误提示；重试后正常回答（457 字符）。日志 `content_chars=0 took_ms=45300` 为网关偶发行为，前端不再静默

### 质量门

- `cargo test` 全绿（161+9+15...）、`tsc -b` 0、scopeParser + translateErr 断言全过、semgrep ERROR 0
- MCP 实测：搜索"陈骥"→2 会话、清空→39；拖拽 224→324px 且 localStorage 持久化

---

## 2026-08-16（第二轮加固 · i18n 全覆盖 + 超时防护 + HTTPS 警告 + 权限收紧）

继续鲁棒性/可用性/安全性修复（第二轮）：

- **后端错误消息 i18n（U2）**：新增 `src/utils/translateErr.ts` —— 后端中文错误消息经映射表转为 i18n 键，前端 `t()` 翻译；未命中键回退原文不回归 zh。接入 `ChatPanel.errText`。4 语言各加 30 个 `err_*` 键 + `app_error`/`retry`/`index_*` 键
- **扫描 metadata 超时防护（R2）**：新增 `scanner/helpers.rs::metadata_timeout`（辅助线程 + recv_timeout，15s/10s），替换 scan 主循环 3 处 `entry.metadata()` 与 `handle_event` 的 `std::fs::metadata`，网络文件系统（NFS/SMB/FUSE）不再可能挂死扫描
- **`mark_extracted` 二次失败告警（R5）**：重试仍失败时打 WARN 日志（注明 Phase 1 进度不可见、下次扫描重试），替代静默 `let _ =`（`src-tauri/src/indexer.rs`）
- **浏览页键盘导航（U6）**：↑↓ 移动选中行并滚动定位，Ctrl/Cmd+A 全选（扩展原有 handler，`src/pages/Browse.tsx`）
- **ErrorBoundary/IndexStatus 硬编码中文 i18n（U3/U4）**：ErrorBoundary 用 `static contextType` 读 i18n context；IndexStatus 验证/重提取消息改 `t()` 模板参数
- **LLM 远程 HTTP 警告（S4）**：`ActiveEndpoint::check_https()` 检测非本地 `http://` 网关，`OnceLock` 防刷屏打 WARN（提示 API Key/文档内容明文传输风险）（`src-tauri/src/ai/mod.rs`）
- **CSP 收紧 + 权限最小化（S5/S6）**：CSP 加 `object-src 'none'` + `base-uri 'self'`；fs scope 移除 `$DOCUMENT/**` `$DESKTOP/**` `$DOWNLOAD/**`（前端 fs 仅用 `writeTextFile` 导出，经 dialog save → `$DIALOG_FILES/**` 覆盖）
- **新增单测**：`src/utils/__tests__/translateErr.test.ts`（zh 保原文/en 翻译/未命中回退/键名不泄漏）
- **测试**：全量 `cargo test` 全绿、`tsc -b` 0、scopeParser + translateErr 断言全过、semgrep ERROR 0

---

## 2026-08-16（鲁棒性 · 可用性 · 安全性全面摸底修复 · 首批 7 项）

基于三路并行审计（鲁棒性/可用性/安全性），修复首批问题：

- **`get_content_ocr_used` 设计缺陷（R1）**：`query_row` 无数据时返回 `QueryReturnedNoRows` 错误，改为 `match` 显式处理该变体返回 `Ok(false)`（`src-tauri/src/db/tracker.rs`)
- **`⌘K` 快捷键不存在（U1）**：搜索框 placeholder 写了 `(⌘K)` 但无监听器。新增 `useEffect` 全局监听 `⌘K`/`Ctrl+K` 聚焦搜索框（`src/components/SearchBar.tsx`）
- **`download_files` 路径穿越（S2）**：原用数据库相对路径直接 `std::fs::read`（相对 CWD，可能被 `../../` 利用）。改为按 `dir_id` 查目录根路径拼接绝对路径再读取（`src-tauri/src/commands/files.rs`）
- **API Key 文件权限（S1 部分）**：`config.json` 写入改为 Unix `mode(0o600)`（仅 owner 读写），Windows 维持默认（`src-tauri/src/config.rs`）
- **ChatPanel 流式监听静默失败（R3）**：`listenAiStream` 失败由 `catch(() => {})` 改为 `console.error` 输出（`src/components/ChatPanel.tsx`）
- **`preview_file_by_path` 路径穿越防护（S3）**：非 DB 路径回退读取前，`canonicalize` + 校验在任一已监控目录内，越界拒绝（`src-tauri/src/commands/files.rs`）
- **Browse 重索引静默吞错误（R4）**：`reindexFiles().catch(() => {})` 改为 `console.error`（`src/pages/Browse.tsx`）
- **测试**：全量 `cargo test` 全绿（161+8+15 等）、`tsc -b` 0、scopeParser 全过、semgrep ERROR 0

---

## 2026-08-15（浏览页优化 · 三波迭代：效率/呈现/体验）

分三波优化浏览页（`src/pages/Browse.tsx` + 4 个 i18n 文件）：

- **波一 · 快速修正**：
  - 右键菜单加「复制路径」（`copy_path` 复用，`navigator.clipboard` 先行例），路径终于可复制（表格原 `select-none` 且菜单无此项）
  - reindex 确认弹窗：仅当目标文件全部已索引时弹 `ask(confirm_reindex)`（未索引直接重建不打扰；`confirm_reindex` 文案 4 语言原本存在却从未被调用）
  - 暗色补齐 4 处：`no_preview_available`/`loading_preview`/`select_file_preview` 加 `dark:text-gray-500`，日志弹窗关闭按钮加 `dark:hover:text-gray-200`
  - 预览面板折叠/展开把手（▶/◀ 切换，w-80 ⇄ 隐藏，表格区恢复 ~975px）
- **波二 · 交互效率**：
  - **列宽持久化**：`colWidths` 从 `useState` 改 `usePersistentState(LS_KEY_COLS)`，重启后保留宽度；`type`/`status` 列补拖拽把手 + 双击 autoFit 支持（原仅 filename/path 可拖，且宽度不持久）
  - Ctrl/⌘+单击加选时预览跟随最后点击行（原只 toggle `selectedIds`，预览停在旧文件）
  - 右键菜单视口 clamp：`min(x, viewportW-190)` / `min(y, viewportH-200)`，贴右下角不再溢出窗口
  - AI 问答错误红色态：新增 `askError` state，失败渲染红底红字，与成功紫底区分
- **波三 · 信息呈现**：
  - 预览轻量增强：文件名标题栏（带 title 提示）；图片缩放按钮组（50%/100%/150%/200%，`transform: scale()`）；超 50k 字符截断显示 + `truncated_notice` 提示（复用 i18n）
  - **图片预览修复**：`get_file_preview`/`get_file_preview_inner` 返回的 `image_path` 原为数据库中的相对路径，`convertFileSrc` 无法加载 asset 协议链接。改为拼接 `dir_config.path`（目录根路径）得到绝对路径，图片预览正常显示（`src-tauri/src/commands/files.rs`）
  - 分页体验：删除 `pageSize` 变化即 `setPage(1)` 的 effect（窄窗口 toolbar 折行/拖拽调整高度不再把用户踢回第 1 页；越界仍由既有 `page > totalPages` clamp 兜底）
- **验证**：`tsc -b` 0、scopeParser 全过、semgrep ERROR 0；MCP 实测——右键菜单贴右下角触发仍完整在视口内（1200×768）、预览标题栏显示选中路径、图片点 200% 生效（`scale(2)` + 按钮高亮）、表头 4 列均可拖
- **待用户确认**：预览折叠按钮在 MCP 驱动下出现渲染层异常（onClick 探针执行但 setState 不更新，同组件其他 setState 正常），代码为教科书式 React（useState + 条件渲染 + toggle），疑似 WebView/MCP 驱动环境问题；需真机点一下确认

---

## 2026-08-15（检索依据文件可点击 → 跳转浏览页预览）

- **AI 聊天「检索依据」文件可点击**：证据面板（`<details>` 内 `<li>` 项）整行可点击，`navigate('/browse?path=<相对路径>')`，带 hover 样式；不触发系统打开（顶部来源栏已有 `openFile` 入口，不重复）（`src/components/ChatPanel.tsx`）
- **Browse 支持 `?path=` 深链**：挂载时读一次 `path` 参数 → `selectFile(path)` 自动打开右侧预览；给表格行加 `data-relpath`，若目标行在当前列表中则 `CSS.escape` 后按属性选择器 `scrollIntoView` 定位高亮。参数经 sync effect 覆盖 URL 后天然一次性消费，不残留（`src/pages/Browse.tsx`）
- **深链列表定位修复（forcedSearch）**：初版只开预览、列表仍显示 localStorage 持久搜索词的结果（列表查 `path LIKE %旧词%`、预览按精确路径查，两通道割裂）。改为 `forcedSearch` state 与持久化 search 分离：挂载时若带 `?path=` 则列表按该路径查询，目标行必现并高亮；用户手动输入搜索词时清空 forcedSearch 让位，持久词不污染（`src/pages/Browse.tsx`）
- **验证**：`tsc -b` 0、scopeParser 全过、semgrep ERROR 0；MCP 实测点击证据项「金昆芳/审计报告.pdf」→ 跳转 Browse → 右侧预览自动打开该文件（187550 字符），未破坏既有过滤状态

---

## 2026-08-15（AI 聊天全面测试 · 修复跨会话泄漏等 4 项 + 新增 13 个自动化测试）

对 AI 聊天功能做全面摸底（真实 API + MCP 驱动六维度：功能/流式/异常/并发/配置/前端体验），修复 4 项问题，补齐 13 个自动化测试。

- **跨会话 @mention chips 泄漏（D-3）**：`ChatPanel` 中 `mentionChips`/`conditionChips`/输入文本为组件本地 state，切换/新建会话时不重置，导致新会话问题错误携带上一会话的 `@文件` 引用、检索范围被污染（实测新会话问"克虏伯项目"回答却只引用旧会话的 JPG 材料）。新增 `useEffect` 在 `session.id` 变化时清空三者（`src/components/ChatPanel.tsx`）
- **设置页 `{n}` 占位符未插值（E-3）**：已启用模型分组标签 `t(labelKey)` 漏传 `{ n }` 参数，页面直接显示 `Embedding ({n})`/`LLM ({n})`。补插值参数（`src/pages/Settings.tsx:676`）
- **模型不可用报错误导（E-2）**：active LLM 指向被删 provider/不存在模型时，`llm_enabled()` 返回 false 且统一报「AI 服务未配置」，掩盖真实原因。新增 `llm_unavailable_reason()` 区分"未配置/网关被删/模型不可用"三种情形，6 处报错点复用（`src-tauri/src/ai/mod.rs`、`src-tauri/src/commands/ai.rs`）
- **MCP E2E 脚本选择器失效（F-1）**：`ai_chat_e2e_mcp.sh` 用 `input[placeholder*="追问"]` 只匹配追问态，新会话首问 placeholder 是"输入问题…"，选择器无匹配致测试失效。改为兼容两种 placeholder（`src-tauri/tests/ai_chat_e2e_mcp.sh`）
- **`chat_stream` wire 层缺测试**：新增 `src-tauri/tests/ai_chat_stream_wire.rs` —— 本地 Mock SSE 网关（std TcpListener）覆盖 8 条路径：正常流聚合、`[DONE]` 终止、网关忽略 stream 回退纯 JSON、畸形帧静默跳过、断流保留部分文本、HTTP 500 降级、空配置降级、连接拒绝降级、请求体校验（model/stream/messages）。`config_dir()` 增加 `LS_CONFIG_DIR` 环境变量覆盖作为测试后门，避免 wire 测试读写真实用户配置（`src-tauri/src/config.rs`）
- **会话持久化缺测试**：`history_tests` 新增 3 个：50 会话上限驱逐（create 时按 updated_at 淘汰最旧）、`save_chat_session` 同 id 原地更新不重复且标题推导幂等、`export_chat_session` Markdown 完整性（标题/轮次/引用/检索依据/改写标记/严格/专注模式）。`save/export/create` 命令体抽出可注入 data_dir 的内部函数（`src-tauri/src/commands/ai.rs`）
- **测试**：全量 `cargo test` 16x 套件全绿（含 8 新增 wire + 3 新增持久化 + 原有 161 单元/集成）、`tsc -b` 0、scopeParser 断言全过
- **冒烟验证**：真实 API 六项全过——首问流式（⏱+检索依据）、多轮追问上下文、中途取消（无残留块）、`@文件` 专注模式、导出 Markdown 结构、语义搜索开关；另发现记录未修：取消对话框用原生 `confirm()` 阻塞自动化且取消不清除 LLM 在途消耗、同秒批量建会话驱逐顺序未定义（秒级时间戳）

---

## 2026-08-13（手册拆分多文件 · step-by-step 重写）

- **手册从单文件拆分为多文件**：`USER_MANUAL.md` 变为索引页，11 章拆分为 `docs/01-install.md` ~ `docs/11-faq.md`，每章独立文件
- **step-by-step 重写**：每章按编号步骤组织（步骤 1、2、3…），每一步含操作说明 + 预期结果 + 截图引用
- **每章新增验证清单**：末尾 `✅ 本章验证清单`，读者可逐项确认完成
- **学习路径重组**：按安装 → 添加资料库 → 等待索引 → 搜索 → 浏览 → 设置 → 索引管理 → AI → 备份 → CLI → FAQ 顺序排列
- **截图标注**：5 张核心截图（搜索页、资料库、索引状态、浏览页、设置页）添加红框 + 编号标注，存入 `docs/screenshots/annotated/`，各章节已更新引用

## 2026-08-13（用户手册重写 · 图文并茂新手向）

- **重写 USER_MANUAL.md**：从技术文档转为新手向图文手册，每节配截图；新增 13 张操作截图（8 页全景 + 设置页 5 tab），全部截取自真实运行环境与演示数据（`docs/screenshots/`）
- **造演示样本数据**：Python 脚本生成 19 个涵盖 12 种格式的文件（md/txt/csv/docx/xlsx/pptx/PDF/扫描件PDF/图片OCR/bin），统一存放于 `~/Documents/Link-Searcher-Demo/`，用于手册截图与功能演示（`gen_demo_data.py` 临时脚本已清理）
- **手册结构**：11 章，含新手引导、搜索入门、AI 增强、资料库、索引管理、设置详解、FAQs 等
- **流程图绘风格化**：5 个 mermaid 流程图先替换为 draw.io 图，后替换为 excalidraw 手绘风格图（`docs/screenshots/excalidraw-*.png`），使用 Excalidraw 8 色语义调色板，Kroki API + rsvg-convert 导出

## 2026-08-13（修复 AI 聊天页打开即崩溃 · 前端 TDZ）

- **点击「AI 聊天」显示"应用出错了"**：`AiChat` 中 `handleSessionChange`（useCallback）声明在其调用者 `handleFocusFile`/`handleScopeAction`/`handleSetSessionScope`/`handleClearSessionScope` 之后，依赖数组在渲染时访问尚未初始化的 `const` 触发 TDZ `ReferenceError`，被错误边界兜底成通用提示。将 `handleSessionChange` 上移至所有调用者之前（`src/pages/AiChat.tsx`）
- **错误边界吞错误**：`ErrorBoundary` 只显示通用文案不显示错误详情，导致无法定位。改为显示 `error.message` + stack + 重试按钮，并 `componentDidCatch` 写 console（`src/components/ErrorBoundary.tsx`）
- **`PerTurnScope` 前端类型未定义**：`ChatSession.per_turn_scopes` 引用未定义的 `PerTurnScope`，`tsc -b` 报 TS2552。补定义接口（turn_index/files/dirs，对齐后端 `commands/ai.rs`）（`src/api/files.ts`）
- **strict_docs toggle 空会话**：`{...session, strict_docs}` 在 session 为 null 时展开为 `{}` 且缺 id，加 `session &&` 守卫（`src/components/ChatPanel.tsx`）
- **dirTrees root 类型错误**：state 声明 `root: DirTreeNode | null` 与 `getDirChildren` 返回 `DirTreeNode[]` 不符，改为 `DirTreeNode[] | null`（`src/pages/AiChat.tsx`）
- **tsc -b 被绕过**：根 tsconfig `"files": []` + project references 导致裸 `tsc --noEmit` 零文件空过；正确检查为 `tsc -b`。`tsconfig.app.json` 排除 `__tests__`（Node 原生 type-stripping 单测，含 `node:assert`）
- **测试**：tsc -b 0，vite build 通过，semgrep ERROR 0，scopeParser 单测 11 断言全过

---

## 2026-08-12（审计复核 + 技术债收尾）

- **审计复核修正**：初版审计的 L3"363 处生产 unwrap"、L4"indexer.rs read().unwrap() 锁中毒"经精确复核（awk 切分 `#[cfg(test)]` 块）均属**误判**——真实生产代码 unwrap 接近 0，仅存几处均有 `nosemgrep` 标注或位于测试辅助函数。L2 oxlint 18 warnings 为 `only-export-components`（i18n/theme 全局 hook 刻意导出）+ `exhaustive-deps`（已加 eslint-disable 注释的刻意抑制），修复风险 > 收益，不动
- **L5 copy_dir 深度限制**：备份目录递归复制加 64 层深度上限，防极端深目录栈溢出（`src-tauri/src/commands/backup.rs`）
- **测试**：157 单元全过，tsc 0，semgrep 0

---

## 2026-08-12（代码审计修复 · H1 私密泄漏 + M1/M2 scopeParser）

- **H1 私密目录过滤漏洞**：P6 只在 `search` 命令过滤了 private 目录，AI 聊天检索路径（`bm25_relevant_hits`/`prepare_smart_prompt`）漏了——私密文件仍会出现在 AI 答案里。抽 `has_private_dirs`/`list_public_dir_ids` 公共函数到 `db/dir_config.rs`，三处检索入口（search、smart_search、conversation）统一在 dir_ids 为空时注入非 private 过滤（`src-tauri/src/db/dir_config.rs`、`src-tauri/src/commands/search.rs`、`src-tauri/src/commands/ai.rs`）
- **M1 URL 破坏**：scopeParser 的 `CMD_TOKEN_RE` 泛化清理把 `https://example.com` 的 `//example.com` 当命令剥离——删除该泛化清理（带冒号的命令已由精确正则剥离，无冒号的 `/xxx` 非合法命令不再误伤）
- **M2 `@@` 前缀污染**：`MENTION_RE` 排除 `@` 字符 + `replaceAll` + 清理孤立 `@`——`@@财务` 不再带入 `@` 前缀
- **L1 unused imports**：清理 IndexStatus、searchFilePaths（`src/pages/IndexStatus.tsx`、`src/pages/AiChat.tsx`、`src/components/ChatPanel.tsx`）
- **测试**：新增 `test_private_dir_filtering` + scopeParser S9(URL)/S10(@@)/S11(句尾@)；157 单元全过，tsc 0，semgrep 0

---

## 2026-08-12（P6 私密目录标记 · 不索引不搜索）

- **P6 私密目录标记（借鉴 Hyperlink 选择性索引）**：`dir_config` 表加 `private` 列（migration + CREATE_TABLES 同步）；`DirConfig`/`DirConfigWithStats` 加 `private` 字段；`DirUpdate` 加 `private` 选项；`update_dir` 命令加 `private` 参数；**搜索过滤**——全库搜索时自动排除 private 目录的文件（`commands/search.rs`）；**前端 DirManager** 每行加「私密/公开」toggle 按钮，点击即保存（`DirManager.tsx`、`api/dirs.ts`）
- **测试**：156 单元 + 9 集成 + 6 IPC + 2 OCR 全过，tsc 0，semgrep 0

---

## 2026-08-12（波 2 · 混合权重可调 + 严格模式 + 专注模式）

- **P1 混合权重可调（借鉴 RAGFlow 0.3/0.7）**：`AppConfig.semantic_weight`（默认 0.3 = 语义30%/关键词70%，serde default 兼容旧配置）；`semantic_fuse` 从等权 RRF 改为**分数级加权混合** `score = w×cosine + (1-w)×bm25_norm`（BM25 归一化到 0~1 与 cosine 同尺度）；抽 `weighted_mix` 纯函数 + 3 单测；设置页 AI 区新增「检索策略」滑杆（`semantic_weight` 全局设置，经 `ConfigInfo` 读写 round-trip）（`src-tauri/src/config.rs`、`src-tauri/src/commands/ai.rs`、`src-tauri/src/commands/config.rs`、`src/pages/Settings.tsx`、`src/api/config.ts`）
- **P2 严格模式（借鉴 AnythingLLM query 模式）**：`ChatSession.strict_docs` 会话级字段；`conversation_ask`/`conversation_ask_stream` 加 `strict_docs` 参数，`prepare_conversation_prompt` 空材料时返回「未在与当前范围匹配的文档中找到依据」而非 LLM 自由发挥；ChatPanel 范围控制行加「仅依据文档」toggle（绿色高亮，会话持久化）（`src-tauri/src/commands/ai.rs`、`src/components/ChatPanel.tsx`、`src/api/files.ts`）
- **P3 专注模式（借鉴 DeepVein 📎）**：`ChatSession.focus_file` 字段；树状浏览器**文件右键「专注分析」** → 会话设 focus_file；发送时若 focus_file 存在则 scope 仅该文件（忽略其他范围）；ChatPanel 范围控制行显示「📌 专注: xxx」chips 可退出（`src/pages/AiChat.tsx`、`src/components/ChatPanel.tsx`）
- **测试**：新增 `weighted_mix_prefers_keyword_when_weight_low`/`prefers_semantic_when_weight_high`/`empty_or_zero_bm25_handled` + `semantic_weight_defaults_to_0_3_and_round_trips`；155 单元 + 9 集成 + 6 IPC + 2 OCR 全过，tsc 0，semgrep 0

---

- **背景**：通盘调研（编程 IDE→知识库→本地检索工具 Hyperlink/MangoFinder/LoFS）+ 审视波 1——砍掉与 `keep_old` 重复的 `@上轮` 继承；保留真正缺口：**会话级目录范围前端口** + **/命令前端解析从未接线**（后端起 ext/date 逻辑但前端硬编码 `conditions: []`，功能不可达）
- **后端**：`conversation_ask`/`conversation_ask_stream` 新增 `session_scope_dir_ids` 参数并传入 `prepare_conversation_prompt`（此前写死 `&[]`）；修复 searcher 测试构造缺 `path_prefixes`（6 处）+ 集成测试 1 处（`src-tauri/src/commands/ai.rs`、`src-tauri/src/search/searcher.rs`、`src-tauri/tests/integration.rs`）
- **前端**：新建 `scopeParser.ts` 纯函数模块——`@mention`（文件/目录/`@上轮`/`@第N轮`）+ `/ext` `/date` `/范围:全库|目录` `/模糊` 解析，返回 scope + 净化文本 + 范围动作；ChatPanel 换用模块并解析 `/ext` `/date` 进 `conditions` 传入后端；新增**条件 chips**（蓝色 `/ext:pdf` 等实时显示可审计）；`/范围` 动作经 `onScopeAction` 回传 AiChat 解析为 `dir_id` 更新会话范围；树状浏览器目录加「范围 ✓/范围」按钮（设为/清除会话级目录范围，持久化 `scope_dir_ids`）（`src/utils/scopeParser.ts`、`src/components/ChatPanel.tsx`、`src/pages/AiChat.tsx`、`src/api/files.ts`）
- **测试**：新增 `scripts/scopeParser.test.ts` 纯函数断言（9 场景：@文件/@目录/@上轮//ext//date//范围全库//范围目录//模糊/无命令）全过；152 单元 + 9 集成 + 6 IPC + 2 OCR 全过，tsc 0，semgrep 0

---

- **问题**：AI 聊天默认全库检索，无法精确控制检索范围（指定文件、文件夹、条件）；追问时范围不可调
- **方案（/grill-me 设计访谈）**：`@mention` 轮询级（默认不继承）+ 显式 `@上轮`/`@第N轮` 继承（传递闭包，循环防护）+ 会话范围只绑目录（替换式）+ 编号引用模型（`@文件` → `[N]`，路径字符串不进 LLM，分句映射精确）+ LLM 不碰范围决定权
- **后端**：`TurnScope`/`PerTurnScope`/`ScopeCondition` 结构；`ChatSession` 新增 `per_turn_scopes`/`scope_dir_ids`/`scope_conditions`（serde default 兼容旧 JSON）；`conversation_ask`/`conversation_ask_stream` 接受 `scope` 参数；`prepare_conversation_prompt` 集成 scope——`@文件` 按路径直用编号进材料、`@目录` 解析为 dir_ids 检索过滤、条件（ext/date）传入 `SearchParams`（`src-tauri/src/commands/ai.rs`）
- **前端**：`api/files.ts` 新增 `TurnScope`/`ScopeCondition` 类型 + `conversationAsk`/`conversationAskStream` 加 `scope` 参数 + `searchFilePaths` API；`ChatPanel` 新增 `parseScope` 函数（正则提取 `@路径` token，按扩展名启发式区分文件/目录，发送时净化文本并将 scope 传入后端）；新建 `MentionPicker` 组件——输入 `@` 时弹出文件/目录选择器（查询后端 `search_file_paths` 命令），键盘导航 + 选中插入路径 token；**范围条 chips**——输入框上方实时显示当前 @mention 路径（文件📄/目录📁），可 × 删除并从输入文本移除（`src/api/files.ts`、`src/components/ChatPanel.tsx`、`src/components/MentionPicker.tsx`）
- **测试**：152 单元 + 9 集成 + 6 IPC + OCR 全过，tsc 0，semgrep 0

---

## 2026-08-12（AI 检索范围 — 树状文件浏览器 + 范围条 chips + @ 选择器）

- **树状文件浏览器**：AI 聊天左侧栏新增可折叠文件树面板——列出所有配置目录，递归显示文件/文件夹；文件左键「加入对话」（插入 `@文件路径` 到输入框）、目录右键「加入对话」（插入 `@目录路径`）；复用 `get_dir_tree`/`listDirs` API + `DirTreeNode` 类型，绝对路径转相对路径与 file_tracking 一致（`src/pages/AiChat.tsx`、`src/components/ChatPanel.tsx`）
- **范围条 chips**：输入框上方实时显示当前 @mention 路径（文件📄/目录📁），选中/输入时同步更新，× 删除同时从输入文本移除 `@path`（`src/components/ChatPanel.tsx`）
- **@ 选择器**：输入 `@` 时弹出文件/目录选择器（查询后端 `search_file_paths` 命令），键盘导航 + 选中插入路径 token；`MentionPicker` 组件（`src/components/MentionPicker.tsx`）
- **i18n**：4 语言各 +4 key（`src/i18n/*.ts`）
- **测试**：152 单元 + 9 集成 + 6 IPC + OCR 全过，tsc 0，semgrep 0

---

## 2026-08-12（AI 回答溯源 + LLM 查询改写）

- **AI 回答溯源（检索依据面板）**：`EvidenceItem` 增加 `bm25_score`/`semantic_score`/`rrf_score`/`rewritten`/`rewritten_query`/`from_history` 六个溯源字段（serde default，旧数据向后兼容）——前端每条助手消息下新增可折叠「🔍 检索依据」面板，展示每份来源的 BM25 分、语义相似度、RRF 融合分、改写标记/改写后查询、历史保留标记（`src-tauri/src/commands/ai.rs`、`src/api/files.ts`、`src/components/ChatPanel.tsx`）
- **分数贯通检索管线**：`bm25_relevant_hits`/`semantic_fuse` 改为返回带分数的 `ScoredHit`（BM25 原始分 + 余弦相似度 + RRF 融合分），替代原丢弃分数的 `(file_id, path)` 二元组（`src-tauri/src/commands/ai.rs`）
- **LLM 查询改写**：`rewrite_query` 保留规则改写兜底（指代词表扩展 `刚才/上面/之前/前面`），新增 `llm_rewrite_query`——命中改写条件且有历史时调一次 LLM（5s 超时 + `valid_rewrite_output` 校验，失败/超时/空输出自动降级回规则改写，每轮 ≤1 次调用），补全"刚才提到的那份报告呢"类上下文指代（`src-tauri/src/commands/ai.rs`）
- **PerTurnEvidence 持久化溯源**：会话每轮证据增加 `items` 字段，检索依据跨会话持久化到 chat_history.json，回放可见（`src-tauri/src/commands/ai.rs`）
- **测试**：新增 `rewrite_query_triggers_on_referential_time_preface`、`valid_rewrite_output_rejects_garbage_and_echoes`、`evidence_item_deserializes_legacy_json_without_scores`，更新 round-trip/rewrite 断言；148 单元 + 9 集成 + 6 IPC + OCR 全通过，tsc 0，semgrep 0

---

## 2026-08-12（追问"出戏"修复 · 来源去噪 + 首轮收敛）

- **现象**：追问"刚才提到的那份报告呢"时回答/来源列表切换为真实索引里的无关文档（如陆家嘴年报），语料文件被稀释到 1/15
- **根因**：① `prepare_conversation_prompt` 旧来源保留逻辑第二遍无差别补槽——把上一轮 `source_ids` 里所有文件（含 9 份弱相关命中）塞满 15 上限，稀释本轮检索命中；② BM25 OR 检索对"年度/报告/财务"类弱区分词全库贪婪召回（4133 命中），单轮 top10 里目标文档占比天然低；③ 首轮 `smart_search` 即携带 10 份来源作为会话起点守卫，污染从第一轮开始
- **修复**：① 旧来源只保留**对话中明确提到过的**（文件名/主文件名出现于消息文本）且硬上限 3 份，删除无差别补槽；② LLM 改写 prompt 要求"最小必要关键词、保留主题实体、去掉 报告/文件/呢/的 等无区分词"；③ 首轮 `smart_search` 来源收敛为 top3（会话起点守卫更干净）（`src-tauri/src/commands/ai.rs`）
- **测试**：149 单元 + 9 集成 + 6 IPC + OCR 全通过，tsc 0，semgrep 0

---

## 2026-08-12（删除操作统一确认）

- **问题**：删除聊天会话、删除 AI Provider 直接执行，无确认——误触即永久丢失（会话消息/检索记录、provider 模型列表缓存）
- **修复**：两处删除操作统一加 `ask` 确认对话框（warning 样式，与已有「清空日志」「重建索引」确认一致）：删除会话提示"消息与检索记录不可恢复"、删除 Provider 提示"模型列表缓存将一并清除"（`src/pages/AiChat.tsx`、`src/pages/Settings.tsx`、4 语言 i18n）
- **盘点**：全项目破坏性操作已确认覆盖——清空日志/重建索引/取消回答此前已有确认，删除会话/删除 Provider 本次补齐；`clearSearchHistory` API 存在但无前端调用点（未接 UI，无遗漏）

---

## 2026-08-12（模型启用精选 · AI 设置页千级模型治理）

- **问题**：provider 模型全量平铺在设置页（9router 单 provider 达 1284 个模型），列表/「当前使用」下拉超长；现有缓解（过滤框 + 类型折叠）在千级模型前失效
- **方案（设计访谈收敛）**：引入**已启用（enabled）精选模型**概念——千级清单退回"折叠 + 搜索"，日常高频操作（当前使用/改类型）只在用户精选的小集合上做
- **后端**：`ModelConfig` 新增 `enabled` 字段（serde default，旧配置兼容）；`auto_enable_first_per_type` 新 provider 自动启用每类型首个（开箱即用）；`refresh_provider_models` 合并时保留 enabled；`set_active_model` **选择即启用**（active 永不指向隐藏模型）；`load_config` reconcile——active 模型自动置 enabled（旧配置迁移）（`src-tauri/src/config.rs`、`src-tauri/src/commands/config.rs`、`src-tauri/src/ai/mod.rs`）
- **前端**：provider 卡片三区——**已启用区**（直接显示，类型下拉 + 停用按钮，当前使用的模型禁停用）/ **全量搜索框** / **折叠的全量列表**（默认折叠、类型标签 + 行内启用按钮，搜索时自动展开）；「当前使用」两个下拉候选 = 已启用 ∩ 类型匹配（原生 select 保留）（`src/pages/Settings.tsx`、`src/api/config.ts`、4 语言 i18n）
- **修复（GUI 实测反馈）**：① 全量列表默认折叠——原 `collapsedGroups` 初始空集导致 `!has(key)` 恒真、全部默认展开，改为 `expandedGroups` 展开集合（默认空 = 折叠）；② 切换「当前使用」模型时先重置该用途可用状态为"检查中"再按新模型重新探测，避免沿用旧模型的可用性显示（`src/pages/Settings.tsx`）
- **测试**：新增 `auto_enable_first_per_type`、`model_config_deserializes_legacy_json_without_enabled`、`reconcile_enabled_active_keeps_active_models_visible`；152 单元 + 9 集成 + 6 IPC + OCR 全过，tsc 0，semgrep 0

---

## 2026-08-12（离线会话回放 · LLM 关闭可审计历史）

- **问题**：LLM 网关关闭后 AI 聊天区被"AI 服务未配置"整块占住（`AiChat.tsx` 以 `aiCap.llm` 为唯一渲染 gate），历史会话无法打开查看——会话审计被迫依赖网关在线
- **修复**：渲染 gate 从 `aiCap.llm` 改为 **`activeSession` 存在即渲染**（`ChatPanel llmEnabled={aiCap.llm}`）；ChatPanel 去掉 llm 早退，LLM 关闭时仍渲染来源栏 + 消息 + 证据面板（只读回放），仅输入区替换为"AI 服务未配置"提示（`src/pages/AiChat.tsx`、`src/components/ChatPanel.tsx`）
- **语义阈值过滤实验（未采纳）**：曾尝试对 RRF 融合候选加余弦阈值（cosine ≥ 0.2）剔除弱相关命中——真实网关实测 bge-m3 对"年度财务报告"类查询的候选余弦普遍 >0.55（含语义无关的南洋商业银行 docx 亦 0.55），**绝对余弦阈值无区分力**，已回滚（记录留档，避免重复尝试）
- **测试**：11 AI 单测 + tsc 0 通过

---

## 2026-08-11（长任务运行态保持 + 完成简报）

- **按钮状态跨页面丢失**：索引页「验证有效性/补齐向量/重提取」等按钮的运行态是组件本地 `useState`——切页面组件卸载 state 重置，任务未完成时按钮恢复可用，可能二次误触。修复：新增**全局任务注册表**（`state.rs` 的 `task_registry` + `TaskGuard` RAII），长任务 start/end 自动登记/移除；`get_index_status` 返回 `running_tasks`，前端按钮从后端状态查（而非本地 state）——切页/刷新仍保持禁用（后端是唯一事实源）
- **TaskGuard RAII**：`track_task`/`untrack_task` 封装为 guard，函数作用域结束（含 early-return/错误路径）自动注销，防任务卡死残留（`src-tauri/src/state.rs`）
- **完成简报**：三个长任务（verify/backfill/reextract）完成时 `push_task_brief`——全局环形缓冲区（cap 50）+ 写 `[TASK]` 前缀日志行（供日志定位）；`get_index_status` 返回 `briefs`（`src-tauri/src/state.rs`、`src-tauri/src/commands/index.rs`）
- **状态栏简报图标**：StatusBar 监听 status 轮询，新 brief 时间戳 > 已读（localStorage 持久化）→ 📋 图标点亮 + 摘要；点击 → 熄灭 + 跳转日志页（`/logs?q=[TASK] verify`）（`src/components/StatusBar.tsx`）
- **日志定位增强**：LogViewer 支持 `?q=` 参数——进入即按关键字过滤 + **自动暂停滚动**（`autoScroll=false`），加「过滤日志」输入框（`src/pages/LogViewer.tsx`）
- **前端按钮改造**：IndexStatus 三个按钮 disabled 从本地 state 改为 `taskActive(running_tasks)`，spinner 同步（`src/pages/IndexStatus.tsx`）
- **测试**：`task_registry_tracks_and_untracks`、`task_guard_drops_on_scope_exit`、`briefs_ring_buffer_newest_first_capped`；145 单元 + 6 IPC 通过，手动 QA（registry 运行中/完成/简报），tsc 0，semgrep 0

---

## 2026-08-11（索引有效性验证 · 空 content 检测 + 自动重试）

- **背景（LO 假成功实证）**：移除 LibreOffice 兜底时发现，曾"成功"经 LO 索引的文件 `content_index.text_content` 实际为**空字符串**（char_count 虚高但无内容）——标记 indexed=1 却搜不到任何内容（真实库验证：233 个可疑文件，仅 8 个是 LO 假成功，其余为历史空索引）。新增**按内容而非状态**验证索引有效性的能力
- **`dead_content` 标记**：`file_tracking` 新列——验证重试后仍为空的文件标 `dead_content=1`，自动验证永久跳过（防反复重试），手动强制可再验；任何成功索引自动清零（`src-tauri/src/db/mod.rs`、`src-tauri/src/db/tracker.rs`）
- **验证规则**：`indexed=1 ∧ trim(text_content) 为空`（含无 content 行）→ 可疑；空即无效（无长度阈值，短文档自然有效）
- **`verify_index_content` 命令**：批量查可疑文件 → 强制跳过 dedup 重新提取（清 content → 删旧 Tantivy 文档 → index_file）→ 复查存储内容：非空 = 恢复（recovered），仍空 = 标记 dead；`force_dead` 参数可强制重验已标记文件（`src-tauri/src/commands/index.rs`）
- **扫描后自动验证**：`trigger_scan` 完成后后台自动跑验证（幂等，dead 文件自动跳过），只处理本次新增/可疑（`src-tauri/src/commands/index.rs`）
- **前端**：索引页「✓ 验证索引有效性」按钮 + 「含已标记文件」复选框（force_dead），展示 检查/恢复/空内容/失败 四计数（`src/pages/IndexStatus.tsx`、`src/api/index.ts`）
- **测试**：`test_dead_content_lifecycle`（标记/排除/成功清零）、`test_find_empty_content_files_picks_truthy_empty`（真文本不触发/空文本触发/无 content 触发）、`verify_core_recovers_or_marks_dead`（端到端：可恢复文件 recovered + 不可恢复标记 dead）；142 单元 + 手动 QA（真实库副本 233 可疑 → dead 后排除正确），tsc 0，semgrep 0

---

## 2026-08-11（彻底弃用 LibreOffice · Office 提取零外部依赖）

- **移除全部 soffice 子进程代码**：`office/mod.rs` 原保留 LoBatcher（请求合并批处理器）、`lo_binary()` 探测、`extract_many_via_libreoffice`、macOS 轮询转换、`lo_fallback` 兜底——全部删除。dispatch 改为**纯原生**：`.doc`→rwml、`.xls/.xlsx`→calamine、`.docx/.ppt/.pptx/.odt/.ods/.odp/.rtf/.epub`→anydoc，失败直接返回原生错误（`src-tauri/src/extractor/office/mod.rs`）
- **依据（日志实证）**：LO 兜底只被垃圾/损坏文件触发（`._` AppleDouble、zip EOCD 损坏、invalid FIB magic、无法识别的 xls），对这类文件 LO 同样失败；正常文件 anydoc/rwml 已全部接管。移除后损坏文件**快速失败**而非 spawn 子进程卡顿
- **配置剥离**：`lo_binary_path`（config.rs/commands/config.rs/ConfigInfo）、`lo_batch_size`（boot.rs 启动读取、settings.rs ALLOWED_KEYS、db seed）全部移除（`src-tauri/src/config.rs`、`src-tauri/src/boot.rs`、`src-tauri/src/commands/{config,settings}.rs`、`src-tauri/src/db/mod.rs`）
- **依赖面板与自检**：依赖列表删除 LibreOffice 条目；文件类型页 Word/Excel/PowerPoint 标为原生支持（不再误导提示装 LO）；启动自检日志去掉 LibreOffice 探测（`src-tauri/src/commands/tesseract.rs`、`src-tauri/src/lib.rs`）
- **前端**：设置页删除「LibreOffice 路径」与「LO 批量大小」区块；i18n 四语移除 `libreoffice_path`/`lo_batch_size`/`lo_batch_desc` 键、更新 `doc_engine_desc`（`src/pages/Settings.tsx`、`src/i18n/{zh,en,ja,ko}.ts`）
- **测试**：新增 `test_corrupt_docx/doc_fails_without_lo_fallback`（断言损坏文件返回 Err 且不再触发 soffice，RED→GREEN）；`doc_rs_poc` 移除 lo_binary 引用；156 测试全过（139 单元 + 9 集成 + 6 IPC + 2 POC），手动 QA 用真实 .doc fixture 验证提取（1142~17929 字符，0.02s，无 soffice）
- **文档**：README/USER_MANUAL 同步（格式表、依赖表、流程图、设置表去除 LibreOffice）

---

## 2026-08-10（设置界面优化 · 分组导航 + 模型列表折叠）

- **设置页从长滚动改为分组 tab 导航**：13 个 section 归 5 组（通用/索引/文档解析/AI/系统），顶部 tab 切换，选中 tab 持久化（`usePersistentState` 记住上次位置）；组内保留 section 标题层级（`src/pages/Settings.tsx`）
- **模型列表过长**：Provider 下模型从全量平铺改为**按类型分组折叠**（Embedding/LLM/未知，带计数，默认收起）+ **名字过滤框**，过滤命中自动展开对应组（`src/pages/Settings.tsx`）
- **硬编码英文全部迁 i18n 四语**：System/Scheduling/Exclusions/OCR 标题、Launch on startup/Auto backup/Exclude patterns 等 label、页面副标题——新增 `tab_*`/`sys_*`/`ocr_lang`/`model_group_*`/`model_filter_placeholder`/`settings_desc` 等 19 键补齐 zh/en/ja/ko（`src/pages/Settings.tsx`、`src/i18n/{zh,en,ja,ko}.ts`）
- **视觉优化**：tab 布局 + 内容容器放宽（max-w-xl→max-w-2xl）提升信息密度
- **测试**：tsc 0 错误、四语键对齐 268/268、semgrep 0

---

## 2026-08-10（代码审核修复 · 模型管理并发安全）

- **Provider CRUD 命令阻塞主线程（UI 冻结）**：`add_provider`/`refresh_provider_models`/`test_provider` 是同步 `#[tauri::command]` 却内部同步调 `list_provider_models`（HTTP 30s 超时），与 backfill 修复前同类 bug。改为 `async fn` + `spawn_blocking`，网络 IO 移出主线程（`src-tauri/src/commands/config.rs`）
- **config.json 读写竞态（丢失更新）**：`load_config`/`save_config` 全文件读改写无互斥，多个命令并发 RMW 会 last-write-wins。新增 `CONFIG_LOCK` 进程内 Mutex 串行化；抽取 `write_config_file`（无锁版供持锁路径调用，避免 std Mutex 非重入死锁）（`src-tauri/src/config.rs`）
- **测试**：138 单元 + 6 IPC 通过；cargo check 0 错误

---

## 2026-08-10（AI 模型管理 · 多 Provider 支持）

- **AI 配置从"各一个"升级为"模型管理"**：`AppConfig` 新增 `providers: Vec<ProviderConfig>` + `active_embedding_model_id`/`active_llm_model_id`（`provider_id:model_id`），可添加/编辑/删除多个 AI Provider，下拉框选择当前使用的 embedding / LLM 模型（`src-tauri/src/config.rs`）
- **模型自动拉取 + 分类**：新增 `list_provider_models`（`GET {base}/models`），按名字启发式分类（`classify_model_by_name`：embed/text-embedding/bge/m3/minilm… → Embedding；instruct/chat/gpt/qwen/deepseek/gemma… → LLM），未命中归 Unknown，用户可在 UI 手动纠偏（`src-tauri/src/ai/mod.rs`）
- **刷新合并不覆盖纠偏**：`refresh_provider_models` 按模型 id 合并——已存在模型的类型以用户手动值为准，新增模型自动分类；拉取失败保留旧列表并返回错误（`src-tauri/src/commands/config.rs`）
- **新命令**：`add_provider`（保存时自动拉取模型，失败不阻塞保存）、`update_provider`、`delete_provider`（使用中的 provider 禁删）、`refresh_provider_models`、`set_active_model`（空串=停用）、`test_provider`（连通性探测）（`src-tauri/src/commands/config.rs`、`src-tauri/src/lib.rs`）
- **AI 运行时全部走 active 反查**：`embed/chat/chat_stream/embedding_enabled/llm_enabled/capabilities/test_gateways` 由 `resolve_active_endpoint` 指向当前选中模型；悬空 active（provider 被改或模型消失）静默降级为未配置（`src-tauri/src/ai/mod.rs`）
- **旧配置自动迁移**：启动时若 `providers` 为空且旧 `embedding_api_base`/`llm_api_base` 非空 → 自动生成默认 Provider + 种子模型记录 + active 指向；旧字段保留兼容（`src-tauri/src/config.rs`）
- **前端设置页重写**：「当前使用」下拉置顶（跨 Provider 合并同类模型，显示能力探测状态）；Provider 行内展开编辑（名称/base_url/掩码 API Key + 小眼睛切换）；行内测试/刷新/删除（在用 provider 禁用）；模型子列表行内类型下拉；「添加 Provider」内联表单自动拉取（`src/pages/Settings.tsx`、`src/api/config.ts`、`src/i18n/{zh,en}.ts`）
- **测试**：`classify_model_by_name_heuristics`、`resolve_active_endpoint_valid_and_dangling`、`migrate_legacy_gateways_seeds_providers_and_active/noop_when_empty`、`provider_find_model_matches_by_id`；129 单元 + 6 IPC 通过，semgrep 0，tsc 0
- **已知 wire 约定**：`ModelType` serde 序列化为 PascalCase（`"Embedding"`/`"Llm"`/`"Unknown"`），前端类型与筛选已按此匹配（内部格式，无外部消费者）

---

## 2026-08-10（AI 回答 Markdown 渲染 —— 引入 remark-gfm 支持 GFM 表格）

- **AI 回答的表格/删除线/任务列表渲染为原始文本**：ReactMarkdown 未启用 GFM 插件（Tables/strikethrough 属 GFM 扩展），LLM 输出的 `| header | ... |` 表格显示为纯管道符文本。安装 `remark-gfm` 并挂载 `remarkPlugins`（`src/components/ChatPanel.tsx`、`package.json`）
- **测试**：tsc 0 错误

---

## 2026-08-10（清理 —— 全部编译警告归零）

- **cargo check 0 warnings**（原 6 个）：删除从未使用的 `IMAGE_EXTRACTOR` static；`extractor/image.rs` 生产路径为测试专用（struct/impl/常量/imports 圈 `#[cfg(test)]`）；`|mut f|`（take 按值）与 `let mut keep_old`（闭包无需可变）去 `mut`；`config.rs` 对引用的无效 `drop(p)` 移除（`src-tauri/src/extractor/{mod,image}.rs`、`commands/ai.rs`、`commands/config.rs`）
- **测试**：129 单元全过（含 image 5 个），tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-11（文档收尾 —— README/USER_MANUAL 同步全路线图变更）

- README：分页改自适应说明、浏览页多选+批量索引、实时监控热重载、索引会话日志、多语言界面、`.doc` rwml 解析、CLI scan/watch 子命令
- USER_MANUAL：§4.2 配置热重载说明、§5 会话日志+批量重新索引小节、§7 设置语言选项（zh/en/ja/ko）、§9 命令行新增 index/scan/watch 子命令与并发写锁提示
- 验证：`cargo check`/`npx tsc`/`semgrep` 零污染（本次纯文档）

---

## 2026-08-11（Wave 4 — 多语言界面 + RAG 内容分析决策）

- **T10 多语言界面(ja/ko)**：新增 `ja.ts`/`ko.ts` 全量字典（各 258 键，diff 确认与 en/zh 键集一致）；`index.tsx` 扩 `Lang` union + 加入配置文件语言白名单；`Settings.tsx` 语言选项加日/韩；`Browse.tsx` 7 处硬编码中文抽为 `t('key')`（`src/i18n/*`、`Settings.tsx`、`Browse.tsx`）
- **T13 RAG 内容分析决策**：路线图 P3 此条**标记为待独立规划**（本地 LLM 摘要/主题聚类/跨文件关联需新接口设计 + 派生数据存储 schema，超出本次批量范围）——ROADMAP.md P3 保留但注明"需独立设计"，不实现
- **整体验证**：138 单元全过、`cargo check` 零 error、`npx tsc` 零错误、`semgrep --severity ERROR` 零发现

---

## 2026-08-11（Wave 3 — 路线图 P2/P3 三项落地）

- **T8 批量 reindex_files**：新增 `reindex_files(file_ids)` 命令（`spawn_blocking` 循环：清 dedup 缓存→`delete_document_only`→`index_file`，防重复文档）；前端 `reindexFiles(ids)` + Browse 从逐文件 `forEach` 改为单次批量 IPC（`commands/index.rs`、`lib.rs`、`api/index.ts`、`Browse.tsx`）
- **T9 update_dir 热重载**：`update_dir` 在 Watch 重启后触发 `spawn_blocking(incremental_scan)`（`is_scanning` compare_exchange 守卫防并发；成功 `emit("scan-completed")`、失败恢复 `is_scanning`）——目录配置（排除/扩展名/OCR 语言/递归）变更即时生效（`commands/dirs.rs`）
- **T11 CLI 增强**：新建 `boot.rs` 共享 bootstrap（pool+init_db+IndexManager+IndexerService+Scanner 组装，`bootstrap_core` 返回 `Result` 供 GUI/CLI 复用）；`cli.rs` 加 `index`（`visible_alias=search`）、`scan [dir]`、`watch dir` 子命令——`scan` 全量扫目录并打印摘要、`watch` 复用 FileWatcher std 线程 + `handle_event` 实时监控（`boot.rs`、`lib.rs`、`cli.rs`）
- **整体验证**：138 单元全过、`cargo check` 零 error、`npx tsc` 零错误、`semgrep --severity ERROR` 零发现

---

## 2026-08-11（Wave 2 — 路线图 P1/P2/P3 三项落地）

- **T3 启动扫描异步化+进度**：`lib.rs` 启动扫描从 `|_|{}` 丢进度改为 `emit("scan-progress", ScanEventPayload)`（复用 trigger_scan 结构供前端统一监听）；新建 `logs/session.rs` 模块（`SessionLog::open/write/close`）附加会话日志契约（`src-tauri/src/logs/session.rs` + `mod.rs`、`lib.rs`、`commands/index.rs`）
- **T7 索引会话日志**：`trigger_scan`/`rebuild_index`/`add_dir` 吸收三处触发点打开/关闭会话日志（`logs/scan-{ts}.log`）；`get_logs` 加 `session_id` 参数安全读取会话文件；新增 `list_session_logs` 命令按 mtime 降序返回会话文件名列表（`commands/index.rs`、`dirs.rs`、`logs.rs`、`lib.rs`）
- **T12 多轮 RAG 补全**：四子项(a) `rewrite_query` 指代改写（`它/这个/上述/该/那` 开头或<4字时拼接最近用户关键词）；(b) `bm25_relevant_hits` 支持 `semantic:true`（embed + RRF 融合 K=60）；(c) `EvidenceItem` 结构化引用（`{file_id, path, snippet}`）加入 `SmartSearchResponse`/`AiDone`/`PreparedSmart`/`PreparedConversation`；(d) `ChatSession` 增加 `per_turn_evidence`（serde default 兼容旧记录，前端 ChatPanel done 处理中记录每轮 source_ids）；+4 单测（`commands/ai.rs`、`api/files.ts`、`ChatPanel.tsx`）
- **整体验证**：138 单元全过、`cargo check` 零 error 零 warning、`npx tsc` 零错误、`semgrep --severity ERROR` 零发现

---

## 2026-08-11（Wave 1 — 路线图 P0/P1/P2 六项落地）

- **T14 音频 STT 过期条目标记**：ROADMAP.md P3 删除已完成的音频 STT 行（`ROADMAP.md`）
- **T1 Semgrep 微清理**：`searcher.rs:505` 静态正则与 `paddleocr.rs:115` OnceLock 各加 `// nosemgrep: rust-unwrap` 显式豁免；`lib.rs:516` `Builder.run().expect` 改为 `match` + `log::error!` + `std::process::exit(1)`（`src-tauri/src/search/searcher.rs`、`extractor/paddleocr.rs`、`lib.rs`）
- **T2 IO 并发上限**：`indexer.rs` `batch_index` Phase-1 的 `par_iter()` 改经 `batch_io_pool(cap)` 专用 Rayon 池（`num_threads=cap`，默认 8，AtomicUsize 可配置，`OnceLock<Mutex<HashMap<>>` 池缓存）；新增 `batch_io_pool_limits_concurrency` 单测验证饱和计数器峰值 ≤ cap+1（`src-tauri/src/indexer.rs`）
- **T4 浏览页动态分页**：`Browse.tsx` `pageSize` 硬编码 20 改为 `useState` + `ResizeObserver` 监听 `tableRef` 容器，实测首个 data row 高度计算 `pageSize=floor(height/rowHeight)`，pageSize 变化时 `setPage(1)`；回退估值 25px 附 `ponytail:` 注释（`src/pages/Browse.tsx`）
- **T5 Tantivy reader 节流**：`search/mod.rs` `reader()` 强制 `reload()` 改为 **Mutex<Instant> 窗口节流**（默认 1s 内跳过显式 reload，依赖 OnCommitWithDelay 自动刷新；可注入时钟 + `reload_count` 测试）；新增 `reader_throttles_reloads_within_window` 与 `reader_reloads_after_window_elapses` 单测（`src-tauri/src/search/mod.rs`）
- **T6 .doc POC 决策门**：`rwml 0.1.1`（`default-features=false`）接入 **KEEP 决策**——6 个真实中文 Word 97 .doc 全部提取成功（1142–17929 chars，与 LO 对照一致），损坏文件零 panic（typed `NotOle2` 错误 → `lo_fallback` 优雅回退）。`office/mod.rs` 加 `"doc"` 独立分支 + `doc_rs_then_lo` 三段链（`src-tauri/Cargo.toml` + `Cargo.lock` + `office/mod.rs`；集成测试 `tests/doc_rs_poc.rs` + `tests/doc_rs_poc_fallback.rs`）
- **整体验证**：132 单元全过、`cargo check` 零 error 零 warning、`npx tsc` 零错误、`semgrep --severity ERROR` 零发现

---

## 2026-08-10（文档 —— 手册补充追问动态依据与模型分类说明）

- USER_MANUAL §7.5：AI 问答流程图的追问分支更新为动态依据；Provider 分类说明改为"无特征默认 LLM"；新增「聊天的动态依据」段落（首问检索/追问重检索+保留提及旧依据/切页续跑）

---

## 2026-08-10（追问来源列表不更新 —— 恢复 effect 自触发防重）

- **追问后「基于 N 份文档」不更新**：日志实锤每轮追问并发两个请求（`conversation_ask_stream` + `conversation_ask`）——`handleSend` 设置 `pending` 后, 恢复挂起请求的 effect 把自己刚发起的请求当成"残留 pending"重跑（非流式, 不携带新来源）→ 与流式 done 竞争写回, 覆盖了来源列表更新
- **修复**：恢复 effect 加自触发防护（`skipResumeRef`）——`handleSend` 置 pending 前打标, 恢复 effect 检测到本组件刚发起则跳过; 仅切页/重启后挂载的残留 pending 才恢复（`src/components/ChatPanel.tsx`）
- **测试**：tsc 0 错误

---

## 2026-08-10（追问旧来源保留策略 —— 对话中提到过的文件优先）

- **旧来源如何保留**：追问依据合并改为「新检索 top-10 优先」→ 旧来源中**对话消息里被提及的文件**（stem/文件名出现在任一条 user/assistant 消息 = 用户实际使用过）最优先保留 → 其余旧来源按最近加入倒序补槽（去重, 总上限 15）（`commands/ai.rs`）
- **测试**：129 单元全过, semgrep ERROR 0 发现

---

## 2026-08-10（追问动态依据修正 —— 新检索优先, 旧来源只补槽位）

- **追问仍"围绕第一轮文档"回答**：上一版动态合并是旧来源优先填满 15 上限——旧 source_ids 随追问只增不减累积, 顶满后新检索文档永远挤不进来, 动态依据退化为固定快照（用户追问"国有土地征收"时库里明明有征收文档, AI 却说"没有"）
- **修复**：合并改为**新检索优先**（BM25 top-10 全进）→ 旧来源补充剩余槽位（去重, 上限 15 内, 仅供上下文底垫）——追问主导依据更新, 对话上下文由 history 消息保留（`commands/ai.rs`）
- **测试**：129 单元全过, semgrep ERROR 0 发现

---

## 2026-08-10（Provider 模型分类兜底 —— 无特征模型默认 LLM）

- **9router 等 provider 的模型（如 `coding`、`openrouter/…`、`agnes/…`）不出现在 LLM 下拉**：`classify_model_by_name` 对无特征命中的模型返回 `Unknown`，前端 LLM 下拉过滤 `model_type === 'Llm'` 看不到
  - **修复①**：分类兜底 `Unknown → Llm`——绝大多数 provider/models 是对话模型；真正无特征的 embedding 极罕见且 UI 可手动改类型（`ai/mod.rs`）
  - **修复②**：`refresh_provider_models` merge 不再保留旧 `Unknown` 分类（否则改 classifier 后老 provider 刷新仍不生效）——Unknown 让位新分类，用户手动改过的非 Unknown 类型保留（`commands/config.rs`）
  - **生效方式**：设置页对该 provider 点一次「刷新模型列表」重新分类；或删除重加
- **测试**：129 单元全过（分类兜底断言更新），semgrep ERROR 0 发现

---

## 2026-08-10（AI 聊天导出无文件 —— fs scope 白名单 + 错误被吞）

- **导出"能用"但找不到文件**：capabilities `fs.scope` 仅白名单 `$APPDATA/$DOCUMENT/$DESKTOP/$DOWNLOAD` 等，保存到白名单外路径（如 `/Volumes/Data`）时 `writeTextFile` 被 scope 拒绝 → 抛错 → AiChat `catch { /* ignore */ }` 静默吞掉 → 无文件也无提示
  - **修复①**：fs.scope 追加 `$DIALOG_FILES/**`（Tauri 对"用户对话框选中的文件"动态授权的 scope 项——save 对话框选任意位置即可写）（`src-tauri/capabilities/default.json`）
  - **修复②**：AiChat 导出 catch 弹 `alert` 显示真实错误，不再静默吞（`src/pages/AiChat.tsx`）
- **测试**：tsc 0 错误，cargo check 通过

---

## 2026-08-10（追问动态依据 —— 来源列表随问题更新）

- **追问仍用首问快照来源,新文件/新问题不改变依据**：`conversation_ask` 只加载固定 `source_ids`。改为**动态依据**：按追问问题（jieba 分词 OR）重新 BM25 检索 top-10，与**仍有效**的旧来源合并（剔除已删除/内容缺失的文件,去重,上限 15）→ 模型依据与来源列表跟随最新问题（`commands/ai.rs`：新增 `bm25_relevant_hits`、`prepare_conversation_prompt` 返回合并后的 `source_ids/source_files`）
- **前端列表更新**：`conversation_ask_stream` 的 `ai-done` 现携带合并后来源 → ChatPanel 已有的来源 patch 逻辑自动更新「基于 N 份文档」（去旧加新）
- **测试**：124 单元全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（AI 流式输出 + 回复耗时标注）

- **流式输出**：网关支持时逐字/逐段实时显示回答，不再等完成一次性输出。新增 `chat_stream`（SSE `data:` 帧解析 + 每帧 emit `ai-chunk` 事件 + 结束 `ai-done`）与流式命令 `smart_search_stream`/`conversation_ask_stream`（`session_id` 分发，Tauri 事件通道）；前端 `listenAiStream` 增量渲染在"思考中"下方，done 写回完整消息。网关忽略 `stream` 时自动回退一次性解析（非流式网关无感兼容）（`ai/mod.rs`、`commands/ai.rs`、`lib.rs`、`src/api/files.ts`、`ChatPanel.tsx`）
- **回复后标注响应耗时**：`ai-done` 携带 `took_ms`（含生成时间），assistant 消息末尾追加 `⏱ 1分37秒`（`ChatPanel.tsx`）
- **兼容**：取消/恢复/计时/持久化(pending)逻辑沿用；检索/组装逻辑抽 `prepare_smart_prompt`/`prepare_conversation_prompt` 供一次性与流式共用
- **测试**：124 单元全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（AI 等待恢复优化 —— 模块级请求注册表, 切回不重复请求）

- **切页/切会话再切回曾重复调用 LLM**：恢复逻辑直接重跑 pending 问题（并发两个请求、白付一次生成）。改为**模块级活跃请求注册表**（`activeAiRequests`，不随组件卸载消失）：同进程内切回时挂接到**原请求的同一个 promise**（等待其结果，不重发）；仅当注册表无记录（app 重启后残留 pending 或已取消）才重发——这是唯一重发场景（`src/components/ChatPanel.tsx`）
- **测试**：tsc 0 错误

---

## 2026-08-10（AI 等待体验 —— 取消总超时 / 思考计时 / 请求可取消 / 状态恢复）

- **① 取消回复超时**：`build_agent` 去掉 300s 总超时 → `timeout_connect 15s`（不可达立即失败）+ `timeout_read 60min`（安全网，非生成限制）——可达但慢的本地模型可完整生成（`ai/mod.rs`）
- **② 思考中 + 经过时间 + 可取消**：`ChatPanel` 加载态改为「⚙ 思考中 mm:ss」每秒计时 + 「取消」按钮（确认后调新命令 `cancel_ai_request` 置一次性取消标志）；后端 `smart_search`/`conversation_ask` 完成时检查标志丢弃结果并返回"请求已取消"；前端用请求标识防迟到响应（`api/files.ts` cancelAiRequest、`ChatPanel.tsx`、`commands/ai.rs`、`lib.rs`）
- **③ 状态恢复**：`ChatSession` 新增持久字段 `pending_query/pending_started_at`（serde default 兼容旧记录）——请求进行中写入会话文件；切页/切会话再切回时加载到 pending 即恢复"思考中 mm:ss"并**自动重跑该问题**，完成/取消后取回最新结果或原始状态（`commands/ai.rs`、`ChatPanel.tsx`）
- **测试**：124 单元全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（AI 探测独立化 —— 页面判定纯配置, 网络探测仅设置页测试）

- **LLM 探测被 embedding 探测拖累 / 页面加载被网络探测阻塞**：`capabilities()` 每次做两个网络探测（embedding 先跑完 LLM 才启动）→ AI 聊天页打开要等探测、embedding 网关挂了就拖死整个页面。LLM 是可选项，不应阻塞
  - **修复①**：`capabilities()` 改为**纯配置判断**（零网络、瞬时）——LLM/embedding 配置了即可用，页面不阻塞（`ai/mod.rs`）
  - **修复②**：`test_gateways()`（设置页「测试」专用）改**线程并行**探测 embedding+llm，总耗时 = max 而非和；LLM 探测超时 15s→30s
- **测试**：124 单元全过，tsc 0 错误

---

## 2026-08-10（AI 长回答截断 —— max_tokens 1024→4096 + 请求超时 120s→300s）

- **本地 omlx（Qwen3.6-35B）回答被截断**：omlx 日志 `finish_reason=length, max_tokens=1024`——`chat()` 的 `max_tokens=1024` 上限太小，计算/分析类回答（如"1600 万收费计算"）写到一半被截断。另 `build_agent` 120s 全请求超时不足以覆盖慢速本地模型（~15 tok/s × 4096 tokens ≈ 275s）（`src-tauri/src/ai/mod.rs`）
- **修复**：`chat()` `max_tokens` 1024 → **4096**；`build_agent` 超时 120s → **300s**（探测用独立 5s/15s 短超时不受影响）
- **测试**：124 单元全过，semgrep ERROR 0 发现

---

## 2026-08-10（AI 问答检索 —— 问句 PhraseQuery 陷阱修复 + 错误透传 + 批量重提取）

- **AI 问答"未找到相关文档内容"实锤根因**：Tantivy QueryParser 把裸多词 CJK 查询解析为 **slop-0 PhraseQuery**（`parse` Debug 实证：`PhraseQuery[(0,小城),(2,律师),(2,律师收费),(4,收费)]`）——要求所有词在文档中按同一连续位置出现。自然语言问句必然 0 命中；短词/精确短语（"小城收费"）位置吻合才碰巧命中（内存索引隔离实验复现）
  - **修复**：新增 `split_query_terms`（jieba 分词 + 显式 `OR` 连接，跳过单字噪声如"的"）——显式 OR 解析为 BooleanQuery 任意词命中；`smart_search` 用它替换整句（`search/schema.rs`、`commands/ai.rs`）。CLI 实测：`小城 OR 律师 OR 收费规则` 命中，整句 0
- **前端吞真实错误**：ChatPanel catch 兼容 Error/字符串/对象透传后端错误，不再一律"请求失败"（`src/components/ChatPanel.tsx`）
- **批量重提取缺失内容**：新命令 `reextract_missing_content`（判定：md5 存在但 content_index 无记录的历史提取失败文件），先删陈旧 Tantivy 文档再重提取防重复文档，`spawn_blocking` 后台执行；索引状态页新增「↻ 重提取缺失内容」按钮（`commands/index.rs`、`lib.rs`、`src/api/index.ts`、`IndexStatus.tsx`）
- **测试**：124 单元全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（AI 探测超时 5s→15s —— combo 网关 4-12s 调度导致探测 flaky）

- **设置页测试/聊天页探测"很难通过、时好时坏"**：程序日志实锤 `timed out reading response`——`ping_post` 探测超时 5s，而 9router combo 模型（oc/deepseek-v4-flash-free）TTFT 通常 4-12s（proxy 日志 `DONE 4197ms~11887ms`）→ 大半探测 5s 内收不到响应被判 false，少数 <5s 碰巧通过。`IN 0 OUT 0` 是 `max_tokens:1` ping 探测的正常表现（HTTP 200 即通过），非故障（`src-tauri/src/ai/mod.rs`）
- **修复**：`ping_post` 增加 timeout 参数——LLM 探测 5s→**15s**（容忍 combo 调度；最大观测 TTFT 11.9s 留 3s 余量），Embedding 探测保持 5s（localhost 秒回）
- **测试**：124 单元全过，semgrep ERROR 0 发现

---

## 2026-08-10（AI 能力判定不一致 —— 缓存 vs 实时 + 静默失败）

- **设置页测试通过但聊天页显示"LLM 网关未配置或未通过测试"**：`capabilities()` 有 30s 缓存（启动时一次失败的探测结果被缓存为 llm=false），而设置页"测试"是实时探测绕过缓存；且 AiChat `aiCapabilities().catch(() => {})` 静默吞错，aiCap 保持初始 false 永久显示不可用（`src-tauri/src/ai/mod.rs`、`src/pages/AiChat.tsx`）
  - **修复①**：`capabilities()` 去掉 30s 缓存，每次实时探测——聊天页与设置页永远一致（`ai/mod.rs`）
  - **修复②**：AiChat mount 时强制刷新探测；探测失败显示「重试」按钮而非静默 false（`AiChat.tsx`）
  - **修复③**：`test_gateways` 每次探测打 INFO 日志（`embedding/llm = ok/detail`）——探测结果可追踪（`ai/mod.rs`）
- **测试**：124 单元全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（AI 全链路日志 —— 消除"无痕失败"盲区）

- **AI 聊天过程无成功路径日志**：`chat()` 只在失败时 warn、`smart_search`/`conversation_ask` 无入口记录，导致异常时 app.log 看不到任何痕迹（曾误判为"请求没发出"）。补全链路 INFO 日志：
  - `chat()`：请求前记 `model`/`user` 字符数，响应后记 `content` 字符数（`src-tauri/src/ai/mod.rs`）
  - `smart_search`：入口记 `query`；BM25 无相关内容时 warn「未找到相关文档内容」（`src-tauri/src/commands/ai.rs`）
  - `conversation_ask`：入口记 `messages`/`source_ids` 数量
- 配合 9router proxy 日志（`succeeded` + `IN 0 OUT 0`）可端到端定位：app 发出请求体 vs proxy/model 侧差异
- **测试**：124 单元全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（AI 聊天「请求失败」—— SSE 流式响应解析兜底）

- **聊天偶发「AI 请求失败（检查网关配置或网络）」**：日志显示根因是 `chat request failed: trailing characters at line 1 column NNNN`——该 OpenAI 兼容网关（router 代理）部分请求仍返回 SSE 流式（`data:{...}` 多帧），即使请求体带 `stream:false`。ureq 读到多帧文本时 serde 单 JSON 解析失败 → chat() 返回 None → 前端报"请求失败"。另有 `timed out reading response`（网关慢，120s 超时）另一形态（`src-tauri/src/ai/mod.rs`）
- **修复（治本，不依赖网关行为）**：`chat()` 解析改为 `parse_chat_response`——先按纯 JSON 解析，失败则回退扫描 `data: {...}` 行取最后一条有效 payload（跳过 `[DONE]`）。任何 OpenAI 兼容网关（流式/非流式）都能拿到回答，不再报"请求失败"；损坏响应仍返回 Err 保持降级语义
- **测试**：新增 3 例（纯 JSON / SSE 多帧取末帧 / 损坏响应 Err）；124 单元 + 9 集成 + 6 IPC + 2 OCR 全过，semgrep ERROR 0 发现

---

## 2026-08-10（AI 聊天发送按钮无响应 —— 旧版 chat_history 迁移 id 不稳定）

- **发送按钮点击无反应（持久）**：`chat_history.json` 为旧版单会话格式时，`read_history` 的 legacy 迁移只在内存包装、**每次调用都生成新随机 UUID** → `list_chat_sessions` 与 `load_chat_session` 两次独立读取 id 必然不同 → load 返回 None → 前端 `activeSession` 置 null 且 `activeId` 锁死 → `handleSend` 静默 return。三个缺陷叠加（`commands/ai.rs`、`AiChat.tsx`、`ChatPanel.tsx`）
  - **修复①（根因）**：legacy 迁移后立即写回文件持久化，后续读取直接走多会话结构，id 稳定（`ai.rs`）
  - **修复②（自愈）**：`loadSession` 收到 null 时释放 `activeId`，让 ensure-session effect 重跑（`AiChat.tsx`）
  - **修复③（观感）**：发送按钮 `disabled` 加 `!session` 条件，不再"可点无反应"（`ChatPanel.tsx`）
- **测试**：新增 `legacy_history_migrates_once_and_keeps_stable_id` 单测（迁移写回 + 二次读取 id 一致 + 文件变多会话结构）；121 单元 + 9 集成 + 6 IPC + 2 OCR 全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（必做项修复 · 未知扩展 OOM / PDF 挂起 / 幽灵记录 / 前端开关）

- **未知扩展名整文件读入内存 OOM**：不在支持列表的 `.mp4`/`.iso`/`.db` 等被 `read_to_string` 整体读入（50GB 视频 → 分配 50GB → 崩溃）。改为有上限读取（10MB，与 text.rs 一致），超限/二进制内容直接判不支持（`extractor/mod.rs`）
- **poppler 命令无超时挂起扫描**：`pdfinfo`/`pdftotext` 用 `.output()` 阻塞，损坏 PDF 可永久挂起 worker。新增 `run_with_timeout`（try_wait 轮询 + 超时 kill），pdfinfo 60s、pdftotext 120s，超时落 lopdf/跳过 fallback（`extractor/pdf.rs`）
- **移位检测 >10MB 幽灵记录**：候选 >10MB 时 `continue` 不删不移，大文件移动后旧记录永久残留。改为大文件候选视为"已移动"（跳过 MD5 比对）直接删旧记录——新路径记录已由 walk 建立（`scanner/mod.rs`）
- **输入框显示被强制小写**：`setQuery` 的 `toLowerCase()` 把用户输入 "PDF" 显示成 "pdf"。移除，查询侧由后端统一小写（`src/hooks/useSearch.ts`）
- **语义开关不触发重搜**：点「✦ 语义」结果纹丝不动（需手动回车）。toggle 经 `toggleSemantic` 立即重搜（`src/hooks/useSearch.ts`）
- **测试**：120 单元 + 9 集成 + 6 IPC + 2 OCR 全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（核心搜索正确性 · 重复文档 / 语义融合源 / 全量扫描查询 / 分页上限）

- **update_indexed 失败致 Tantivy 重复文档**：`add_document` 成功后 DB 写失败，文档已入索引而记录保持 pending → 每次扫描重试再 add、重复累积。失败分支补 `delete_document` 回滚，索引与 DB 恢复原子（`indexer.rs`）
- **语义重排 BM25 只取当前页**：RRF 融合的 BM25 侧 rank 来自已分页的 20 条（page-local），第 2+ 页命中从不参与融合、排序失真。rerank 前先跑全库 top-100 BM25（同过滤不分页）作融合源，worker 融合后按页码分页——各页结果一致（`commands/search.rs`）
- **full_scan 逐文件索引查询 O(N)**：每文件一条 `get_file_by_path`（每文件 prepare+SELECT），incremental 已是 bulk HashMap。full_scan 改为循环前一次批量载入 `HashMap<rel_path, FileRecord>`，循环内 O(1) 查内存（`scanner/mod.rs`）
- **page 未限幅 OOM**：`page` 无上限，`limit = page*page_size`（≤1000）可被请求到 10⁹ 级 `TopDocs::with_limit`。`page.clamp(1, 10_000)` 封顶（20/页 → 20 万条翻页上限）（`commands/search.rs`）
- **测试**：120 单元 + 9 集成 + 6 IPC + 2 OCR 全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（P2 一致性修复 · Office 扩展名 / 大小写搜索 / 取消不标失败 / 移位检测）

- **Office 新格式永不索引**：`docm/xlsm/xlsb/pptm/ppsm/ppsx/pps/pot` 在 office 提取器支持但 `extract_text`/`classify_ext`/`get_supported_extensions` dispatch 缺失，被当未知格式跳过。三处补齐 8 个扩展名（`extractor/mod.rs`）
- **大写英文搜不到**：jieba tokenizer 未 lowercase 而查询统一小写 → "Report.pdf" 索引 token "Report" 匹配不到查询 "report"。tokenizer 加 `LowerCaser`，索引侧与查询侧归一（中文不受影响）；`filename:` 从整词 TermQuery 改为 RegexQuery `(?i).*xxx.*`——任意位置子串 + 大小写不敏感，兑现 README"正则匹配任意位置"（`search/schema.rs`、`search/searcher.rs`）
  - ⚠️ **存量索引需「索引重建」后大小写搜索才完全生效**（tokenizer 配置变更不影响旧数据）
- **取消扫描把文件永久标 failed**：phase1 取消返回 "scan cancelled"，phase2 统一 `mark_failed` → indexed=2 且不在 needs_reindex 列表 → 永久卡失败。取消项改为跳过标记，保持 pending 待下次扫描自动重试（`indexer.rs`）
- **include_exts 过滤误删记录**：删检测用 disk_set（仅含被允许 ext）比对，磁盘上被过滤的文件其记录被当删除。三处删检测跳过「被 include_exts 过滤且磁盘仍存在」的记录（`scanner/mod.rs`）
- **文件移位检测死代码**：walk 已为新路径建记录 → guard `is_none()` 恒 false → MD5 匹配分支永不执行，moved 恒 0、移动文件被当删除+重索引。去掉恒 false guard，MD5 匹配时硬删新记录 B（`hard_delete_file` + `delete_document_only`）、原记录 A 接管新路径——A 保留 md5/内容**不重提取**，兑现 README"移位检测不重提取"（`scanner/mod.rs`、`db/tracker.rs`、`indexer.rs`）
- **测试**：120 单元（+2：hard_delete 释放 UNIQUE path、filename 任意位置/大小写）+ 9 集成 + 6 IPC + 2 OCR 全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（P1 健壮性修复 · 扫描容错 / 音频与 OCR 超时 / 幽灵嵌入清理）

- **单目录无权限导致整个扫描 abort**：三处 walkdir 迭代错误与 `entry.metadata()` 错误都经 `?` 向上传播，`~/Library` 等一个无权限子目录就让全量/增量/启动扫描整体失败。改为错误计入 `errors` 统计并继续其余文件（`scanner/mod.rs`）
- **ffmpeg 解码无超时 + 整段解码 OOM**：`.status()` 阻塞可永久挂起扫描 worker；无上限解码使 3h 播客膨胀到 ~1.4GB `Vec<f32>`。加 `-t 1800`（30 分钟）截断 + `try_wait` 轮询 300s 超时 kill（`extractor/audio.rs`）
- **tesseract 超时分支死代码**：`recv_timeout(...).context(...)?` 在超时时直接 `?` 传播，pkill+清理分支永不执行 → 挂死的进程/线程/临时图片全泄漏。改为 `try_wait` 轮询 120s 超时，超时 kill+wait+删临时图，并移除 `pkill` 全杀副作用（`extractor/ocr.rs`）
- **重建索引残留幽灵嵌入**：`rebuild_index` 只清 `file_tracking`/`content_index`，`doc_embeddings`/`doc_summaries` 残留行以 RRF 1/(60+rank) 污染语义搜索排序。抽 `clear_index_tables` 四表同清，配单测（`commands/index.rs`）
- **删除文件不清嵌入**：`delete_file` 补调 `tracker::delete_embedding`，删除后不留幽灵参与排序（`indexer.rs`）
- **测试**：118 单元 + 9 集成 + 6 IPC + 2 OCR 全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（P0 审计修复 · watcher 不索引 / 导出损坏 / 目录筛选失效 / 压缩包安全）

- **文件监视器新文件永不索引**：`handle_event` 在 `upsert_file` 写入 mtime 后再 `SELECT mtime` 比较，两次必然相等 → 所有 Create/Modify 事件被当虚假事件吞掉。改为用 upsert **前**的 DB 记录判定（已索引且 mtime/size 未变才跳过），抽 `should_skip_watcher_event` 并配判定表单测（`scanner/mod.rs`）
- **导出 CSV 双重损坏**：后端返回 TempDir 内路径但 RAII drop 即删文件，且前端把路径字符串当内容写入。后端改为直接返回内容字符串，删除临时文件逻辑，前端零改动（`commands/search.rs`、`tests/test_pdf_ocr.rs`）
- **目录树筛选永远返回空**：前端传绝对路径、DB 存相对路径，`LIKE '绝对%'` 永不匹配。新增 `resolve_dir_paths`：绝对路径映射到所属 `(dir_id, rel)`，匹配 `path=? OR path LIKE 'rel/%'`，顺带修掉 `docs` 误匹配 `docs2/` 的前缀边界 bug；search 与 export 共用，配内存 DB 单测（`commands/search.rs`）
- **压缩包 zip-slip 路径穿越（CWE-22）**：归档条目名直接 join 临时目录可经 `../`/绝对路径写出。`append_entry` 写盘前校验 `is_safe_archive_name`（拒绝 `..` 段与绝对路径），拒绝时标记"危险路径"跳过（`extractor/archive.rs`）
- **压缩包解压无上限（zip bomb）**：上限只查声明大小，`read_to_end` 实际字节不限。抽 `read_capped`（`take(cap+1)` 按**实际**解压字节封顶），zip/tar/单文件压缩三处应用，超限跳过/报错、内存有界（`extractor/archive.rs`）
- **pre-existing 测试 bug**：`test_pdf_ocr.rs` 用 `&text[..500]` 切 CJK 文本触发非字符边界 panic，改 `floor_char_boundary(500)`
- **测试**：117 单元 + 9 集成 + 6 IPC + 2 OCR 全过，tsc 0 错误，semgrep ERROR 0 发现

---

## 2026-08-10（AI 聊天请求失败 · 9router 流式响应 JSON 解析）

- **AI 聊天回答「请求失败」但网关测试通过**：9router 等 OpenAI 兼容网关默认返回 **SSE 流式响应**（`data:{...}\n` 多行），`chat()` 的 serde 当单个 JSON 解析 → `trailing characters` 错误 → 返回 None → 前端报失败。服务器端 OUT 0 是流式空 delta（`src-tauri/src/ai/mod.rs`）
- **修复**：`chat()` 请求体加 `stream: false` 强制非流式单 JSON

---

## 2026-08-10（AI 对话管理 · 多会话/自动标题/导出/删除）

- **多会话**：`chat_history.json` 升级为会话数组——`list/create/delete/load/save_chat_session` 命令，左侧栏会话列表（新建 ✅/切换/删除 🗑），上限 50 个，最近优先
- **自动标题**：保存时若会话无标题，取首条用户消息前 20 字
- **导出 Markdown**：`export_chat_session` 生成「问/答」转写 + 引用文件清单，前端 `save` 对话框落地
- **前端**：AiChat 页左侧会话侧栏 + 聊天区；ChatPanel 改为受控组件（session/onSessionChange）管理
- **测试**：113 单元 + 3 smoke + 11 集成通过，build 通过，semgrep 0

---

## 2026-08-10（AI 聊天体验增强 · 持久化/来源/Markdown/打开）

- **对话持久化**：新增 `save_chat_history`/`load_chat_history` 命令——会话存 `data_dir/chat_history.json`，重启恢复（`src-tauri/src/commands/ai.rs`、`src-tauri/src/lib.rs`）
- **来源文件可见**：顶部「基于 n 份文档」可展开为文件列表（来源是相对路径，显示可读文件名）；跟随响应的 `source_ids`/`source_files`
- **Markdown 渲染**：引入 `react-markdown`，assistant 回答渲染加粗/列表/代码块，用户消息保持纯文本（`src/components/ChatPanel.tsx`）
- **点击打开引用**：来源列表项点击 → `openFile(source_id)` 打开对应文件
- **测试**：113 单元 + 3 smoke + 11 集成通过，build 通过（bundle +117KB），semgrep 0

---

## 2026-08-09（语义命中词重叠定位 · 五笔输入修复）

- **语义命中无法定位"为什么相关"**：`semantic_snippet` 用整词子串匹配——查询"物业费"不是文档"物业管理费"的子串（jieba 切成 物业/管理费），无匹配就回退开头、无高亮。改为**词重叠定位**：jieba 分词查询 + 文档，查询词取 2 字前缀（"物业"）匹配文档词，高亮含重叠的完整文档词——搜"物业费纠纷"可高亮"物业管理费合同案件"（`src-tauri/src/commands/search.rs`）
- **防整段误高亮**：初版按"CJK 连续串"捕获，纯中文文档会整句高亮。改为**按文档 jieba 词**匹配，仅高亮单个复合词
- **五笔输入被打断**：搜索框 `onChange` 在每个 IME 组合击键触发 suggest，五笔编码被记录/干扰。加 `onCompositionStart/End`——组合期间不触发 `onFetchSuggestions`，结束后补一次（`src/components/SearchBar.tsx`）
- **测试**：`composite_word_overlap_highlighted`（用户场景）+ 6 项 snippet 测试通过；113 单元 + 3 smoke + 11 集成，semgrep 0

---

## 2026-08-09（语义向量回填 · 免重索引生成向量）

- **首次启用 AI 需重新索引**：向量只在 `batch_index` 内联生成，历史已索引文件（如 8500+）配好网关后必须全量重扫才能获得语义搜索能力。新增**回填命令** `backfill_embeddings`——从 `content_index` 按 md5 读取已提取文本（**不重提娶、不重跑 OCR**），`embed_batched` 分批（64/批）embed 后幂等 upsert 到 `doc_embeddings`，只补缺失向量（`src-tauri/src/commands/index.rs`）
- **扫描结束自动回填**：`trigger_scan`/`rebuild_index` 完成后，若 embedding 网关已配置，后台线程自动跑回填——增量索引新文件即时获得向量，无需手动（`src-tauri/src/commands/index.rs`）
- **移除索引期串行 embedding**：删除 `indexer.rs` 内每文件 `ai::embed()` HTTP 调用（AI 网关开启时会把批量索引拖成龟速且失败 120s 超时），向量生成职责完全移交回填（`src-tauri/src/indexer.rs`）
- **embed 输入截断 + 分批**：`embed_batch` 统一截断到 2000 字符（embedding 模型 512 token 窗口，超长文本会导致网关拒绝整批）；新增 `embed_batched(texts, batch_size)` 分批入口（`src-tauri/src/ai/mod.rs`）
- **前端**：索引页新增「✦ 补齐语义向量」按钮（AI 未配置置灰），展示补齐/失败数（`src/pages/IndexStatus.tsx`、`src/api/index.ts`）
- **修复 UI 无响应**：`backfill_embeddings` 实现为同步 `#[tauri::command]` 会在主线程跑完全部批量 HTTP（8500 文件 × 分批），阻塞 UI。改为 `async fn` + `spawn_blocking`（与 `trigger_scan` 同模式），阻塞工作在后台线程（`src-tauri/src/commands/index.rs`）
- **USER_MANUAL**：7.5 AI 功能「语义搜索」更新为"补齐语义向量，无需重新索引"（`USER_MANUAL.md`）
- **测试**：`backfill_picks_only_indexed_files_without_embedding`（缺口 SQL：只选 indexed=1 ∧ 无向量 ∧ 有文本）、`truncate_for_embed_caps_length`；108 单元 + 3 smoke + 9 集成（含 pre-existing OCR 测试失败 1 项）+ 6 IPC 通过，semgrep 0
- **已知预先存在问题（非本次引入）**：`tests/test_pdf_ocr.rs::test_ocr_20111201` 用 `&text[..500]` 字节切片预览中文文本，OCR 成功（4913 字符）但打印时 panic——修复项独立于本次

---

## 2026-08-09（OCR 并发闸门 · 扫描件索引提速）

- **扫描 PDF 索引过慢（单页 2.5~3.3s）**：根因是双层 `par_iter` 扇出（`batch_index` 文件级 × PDF 页级）叠加无并发控制的 Apple Vision 推理，几十路子进程/推理同时争抢 CPU。修复：`ocr.rs` 新增**全局并发闸门** `OcrGate`（阻塞信号量，上限 `min(CPU核数, 8)` 硬件自适应），`ocr_image_with_engine` / `ocr_image_with_regions` 两个分发入口统一持槽——一处加锁覆盖四引擎全路径（PaddleOCR/Apple Vision/Windows OCR/Tesseract），消除 OCR 过载（`src-tauri/src/extractor/ocr.rs`）
- **删 `ocr_concurrent` 设置项**：该键只作用于 PaddleOCR 内部池却误导用户，且不再读取。清除 seed（`db/mod.rs`）、白名单（`commands/settings.rs`）、设置页 UI（`Settings.tsx`）；PaddleOCR 池固定 2 引擎，Apple Vision 由全局闸门按核数限流
- **数据佐证**：真实库 5734 个 PDF 实测分类——59.7% 纯文字层（直接读）、35.8% 纯扫描件（整本 OCR）、3.2% 混合型；需 OCR 页约 23K。据此**否决**了页级混合提取方案（混合型占比过小，收益 <2% 且引入 wm/garbled 回归风险），保留现有 pdf-inspector + 水印/乱码/重复检测门禁不动
- **测试**：`test_ocr_gate_limits_concurrency`（12 线程抢 3 槽峰值 ≤3）；103 单元 + 3 smoke + 9 集成通过，semgrep 0
- **修复过时断言**：`test_ipc_get_settings` 断言 `ocr_lang=eng` 但 seed 早已是 `chi_sim`（f4f1803 改默认语言时漏同步测试），断言改为 `chi_sim`；同步修正 `file_tracking` 无关的 `app_settings` DDL 兜底 `DEFAULT 'eng' → 'chi_sim'`（`src-tauri/tests/ipc_test.rs`、`src-tauri/src/db/mod.rs`）

---

## 2026-08-08（语义命中预览修复 · 无字面词时回退文档开头）

- **语义命中仍看不到内容/高亮**：`semantic_snippet` 对"词未字面出现"（纯语义同义命中的常见情况）返回空串 → 前端预览为空。改为：无字面命中词时**回退文档开头 ~100 字**（无 `<em>` 但可见内容，用户能判断是什么文件）；字面命中时仍输出 `<em>` 高亮窗口
- **新 `head_snippet`**：首 100 字符、char-boundary 安全（防中文截断）、末尾省略号（`src-tauri/src/commands/search.rs`）
- **测试**：`snippet_tests` 更新为 5 项（首词高亮 / 无字词回退开头 / head 截断安全 / 多字节边界 / 空查询回退）；112 单元 + 3 smoke + 11 集成通过，semgrep 0

---

## 2026-08-08（语义搜索命中可解释 · 高亮摘要 + 相似度得分）

- **语义命中文档无高亮、得分 0**：`semantic_rerank` 从 DB 补的语义命中（BM25 未命中，纯语义召回）设 `snippet=""`、`score=0.0`——用户看不到"为什么命中"。改为：语义命中 **score = 余弦相似度 × 100**（关键字命中的仍用 BM25 分），**snippet 取文档内容中含首个查询词的窗口并用 `<em>` 高亮**（`src-tauri/src/commands/search.rs`）
- **新 `semantic_snippet`**：找查询首个词（大小写不敏感）→ 取 ±60 字符窗口 → `<em>` 包命中词 → 中文 char-boundary 对齐防截断 panic；词未字面出现（纯语义同义命中）返回空
- **测试**：`snippet_tests` 4 项（首词高亮 / 无字面词空 / 多字节边界 / 空查询）；111 单元 + 3 smoke + 11 集成通过，semgrep 0

---

## 2026-08-08（修复 · 语义搜索补充召回失效）

- **整句语义查询（如"关于物业费的诉讼"）搜不到结果**：`semantic_rerank` 只在 **BM25 命中集**上重排，语义 Top-N 中 BM25 未命中的文档被 `filter_map` 丢弃——语义搜索实际只是"关键词候选重排"，同义不同词（纯语义）的文档永远进不来
- **修复**：语义融合序中不在 BM25 里的 id 从 DB `file_tracking` 补充元数据（file_name/path/mtime/size）构造 SearchHit，与 BM25 命中的合并后 RRF 排序——语义补充召回生效，整句/近义查询可命中（`src-tauri/src/commands/search.rs`）
- **测试**：107 单元 + 3 smoke + 11 集成通过，semgrep 0

---

## 2026-08-08（文档 · 语义搜索使用方法增强）

- **USER_MANUAL 新增 2.8 语义搜索章节**：概念（按意思搜）+ 例子（欠费催缴→逾期未缴纳）+ 使用前提（配置 Embedding 网关 / 测试连接 / 触发扫描生成向量）+ 步骤 + 与普通搜索对比表 + 注意事项（未配置/测试失败时的降级行为）
- **设置表更新**：AI 服务项从旧单网关（API Base URL/Key）改为双网关 6 项（Embedding 与 LLM 各自 Base/Key/Model）+ 测试连接；7.5 节同步说明两组独立网关、可用性置灰降级

---

## 2026-08-08（AI 命令异步化 · 修复测试/摘要/问答阻塞 UI）

- **AI 测试/摘要/问答阻塞 UI**：`test_ai_gateway`、`ai_capabilities`、`summarize_file`、`ask_documents` 均为同步命令，内部 `ureq` HTTP 阻塞调用直接跑在 Tauri 主线程——网关不通时（默认 120s 超时）界面冻结。改为 `async` + `tokio::task::spawn_blocking`，HTTP 移到工作线程，UI 保持响应（`src-tauri/src/commands/ai.rs`）
- **ping 专用短超时**：`ping_post` 探测用 5s 超时（而非 120s）——死/挂网关快速失败，测试与启动能力探测不再卡住（`src-tauri/src/ai/mod.rs`）
- **测试环境无关化**：`ai` 单测原假设"测试环境无网关"（断言反应该 None），若本机已配置真实网关则误测真实远程。改为环境无关断言（未配置→降级断言；已配置→仅验证不 panic/不挂起）（`src-tauri/src/ai/mod.rs`）
- **测试**：105 单元 + 3 smoke + 11 集成通过，semgrep 0

---

## 2026-08-08（搜索历史限 10 条 + 一键清除）

- **最近搜索无上限膨胀**：`add_entry` 每次搜索都插入、从不清理，历史越积越多。现每次插入后**修剪到最近 10 条**（pinned 置顶项不受限，只删非 pinned 溢出）；`get_search_history` limit 同步为 10（`src-tauri/src/db/search_history.rs`、`src-tauri/src/commands/search.rs`）
- **created_at 严格递增**：修复 同一毫秒多条插入时 created_at 相同 → 修剪边界随机删的隐患（`now.max(last+1)`）（`src-tauri/src/db/search_history.rs`）
- **一键清除**：新命令 `clear_search_history`（全清含 pinned）+ SearchBar 历史标题旁「清除历史」按钮（前端清除后本地 state 立即清空）（`src-tauri/src/commands/search.rs`、`src-tauri/src/lib.rs`、`src/api/search.ts`、`src/components/SearchBar.tsx`、`src/i18n/*`）
- **测试**：`test_add_trims_to_ten`（15 条→10，最新在前）、`test_add_and_list` 通过；105 单元 + 3 smoke + 11 集成通过，semgrep 0

---

## 2026-08-08（AI 网关拆分 · 可用性测试与降级）

- **Embedding / LLM 网关独立配置**：`AppConfig` 从单 `ai_api_base/key` 拆为 `embedding_api_base/key` + `llm_api_base/key` 两组（各自模型名）；旧单网关配置自动迁移到两组；`update_config` 回写 legacy 字段向后兼容（`src-tauri/src/config.rs`、`src-tauri/src/commands/config.rs`）
- **可用性测试**：新命令 `test_ai_gateway`（ping `/embeddings` + `/chat/completions`，返回各自 `ok/detail`）+ `ai_capabilities`（30s 缓存测试结果）；设置页「AI 服务」拆成 Embedding/LLM 两块配置 + 「测试连接」按钮实时显示 ✓/✗（`src-tauri/src/ai/mod.rs`、`src-tauri/src/commands/ai.rs`、`src/pages/Settings.tsx`）
- **分级降级**：搜索页「✦ 语义」仅在 embedding 网关可用时启用；PreviewPanel 摘要按钮、Browse 问答条仅在 LLM 网关可用时启用；不可用时禁用并 tooltip 引导去设置页配置（`src/pages/SearchPage.tsx`、`src/components/PreviewPanel.tsx`、`src/pages/Browse.tsx`）
- **测试**：`gateways_unconfigured_report_not_ok`（未配置降级）；104 单元 + 3 smoke + 11 集成通过，semgrep 0
- 注：`tests/test_pdf_ocr.rs` 的 `test_ocr_20111201` 为**预存**中文字节截断 bug（`&text[..500]` 切在多字节字符中间,8-04 引入,本次未触）

---

## 2026-08-08（索引完整性 · 分块流水线 + 对账命令）

- **Phase 1 阶段无法搜索**：`batch_index` 原为串行两阶段（全部提取完才一次性写 Tantivy），Phase 1 期间索引无可搜文档。改为**分块流水线**（每 250 个文件：并行提取 → 立即写 Tantivy → commit），已提交的块**立即可搜索**，进度持续可见（`src-tauri/src/indexer.rs`）
- **indexed=3 孤儿文件**（提取完成但 Tantivy 未写——崩溃留给）：`IndexedState` 补 `Extracted=3` 变体；`needs_reindex` 将 3 视为未完成、`get_files_needing_index` 含 3，扫描自动重进队列（`src-tauri/src/db/tracker.rs`、`src-tauri/src/scanner/helpers.rs`）
- **索引完整性对账**：新命令 `check_index_integrity`——统计 DB indexed=1 数与 Tantivy 文档数差异（正差=孤儿），并将残留在 indexed=3 的文件回拨 pending，下次扫描重写（`src-tauri/src/commands/index.rs`、`src-tauri/src/lib.rs`）
- **测试**：`needs_reindex_treats_extracted_as_incomplete`（indexed=1 无需重扫 / 3 需重扫 / mtime 变化重扫）；102 单元 + 3 smoke + 9 集成通过，semgrep 0

---

## 2026-08-08（RAG 二期 · AI 摘要与跨文件问答）

- **`ai` 模块补 `chat()`**：OpenAI 兼容 `/chat/completions`，system/user 双提示，temperature=0.3，未配置/失败优雅返回 None（`src-tauri/src/ai/mod.rs`）
- **`doc_summaries` 表**：`upsert_summary`/`get_summary` 存取缓存（`src-tauri/src/db/mod.rs`、`src-tauri/src/db/tracker.rs`）
- **新命令**：`summarize_file`（单文件 AI 摘要，缓存命中即返回）+ `ask_documents`（多文件 RAG 问答，文本截断防超长，均优雅降级）（`src-tauri/src/commands/ai.rs`、`src-tauri/src/lib.rs`）
- **前端**：PreviewPanel「✦ AI 摘要」按钮（缓存结果展示于文档头部）；Browse 页分页栏上方「AI 问答条」——多选 N 文件后输入问题，回答显示在条内；i18n 新增 `ai_summarize`/`ask_ai`/`ask_selected`/`ask_select_files`（`src/components/PreviewPanel.tsx`、`src/pages/Browse.tsx`、`src/api/files.ts`、`src/i18n/*`）
- **文档**：README「AI 增强（可选）」小节 + USER_MANUAL「7.5 AI 功能」（含隐私提示与重新索引说明）
- **测试**：`chat_degrades_when_unconfigured` 降级断言；101 单元 + 3 smoke + 9 集成通过，semgrep 0

---

## 2026-08-08（向量搜索 · AI 网关 · 语义搜索一期）

- **AI 网关配置**：`AppConfig` 扩展 `ai_api_base/ai_api_key/embedding_model/llm_model`（`#[serde(default)]` 兼容旧配置），设置页新增「AI 服务」段（OpenAI 兼容协议：Ollama/OneAPI/vLLM 通用，留空关闭）（`src-tauri/src/config.rs`、`src-tauri/src/commands/config.rs`、`src/api/config.ts`、`src/pages/Settings.tsx`）
- **新 `ai` 模块**：ureq 调 `/embeddings`，代理/IPv4 环境兼容，401/超时优雅降级；cosine/normalize 向量工具（`src-tauri/src/ai/mod.rs`）
- **向量存储**：`doc_embeddings(file_id, dim, vector BLOB)` 表 + upsert/get_all/delete/count；批量索引 Phase 2 写 Tantivy 时同步生成 embedding（AI 未配置则零开销跳过）（`src-tauri/src/db/tracker.rs`、`src-tauri/src/indexer.rs`）
- **语义搜索**：`search` 命令新增 `semantic` 参数——query embed → 全库向量 cosine → 与 BM25 **RRF 融合**重排；无配置/无向量时自动降级纯关键词（`src-tauri/src/commands/search.rs`、`src-tauri/src/search/searcher.rs`）
- **前端**：搜索页「✦ 语义」开关（localStorage 持久化），i18n zh/en（`src/hooks/useSearch.ts`、`src/pages/SearchPage.tsx`、`src/i18n/*`）
- **测试**：6 个 ai 单测（cosine/归一化/未配置降级）+ embedding DB 往返；100 单元 + 3 smoke + 9 集成通过

---

## 2026-08-08（搜索 vs 浏览不一致 · indexed=3 状态语义修正）

- **已提取文件能预览但搜不到**：批量索引 Phase 1 提取完成即标 `indexed=3`，UI（Browse 状态徽章、`filter=indexed` 筛选、索引统计）把它当"已索引"显示——但 Tantivy 全文索引要到 Phase 2 才写入。结果是浏览/预览有内容、搜索无结果
- **修复**：`indexed=3` 全链路改为中间态——统计 `indexed` 只计 `=1`（可搜索）、`pending` 含 `0,3`；`filter=indexed` 只查 `=1`、`pending` 查 `0,3`；Browse 徽章 `=3` 显示黄色"索引中"（tooltip 提示等待写入索引）；i18n 新增 `extracted_but_not_indexed`（`src-tauri/src/db/tracker.rs`、`src-tauri/src/commands/files.rs`、`src/pages/Browse.tsx`、`src/i18n/zh.ts`、`src/i18n/en.ts`）
- 附：**P0 PdfExtractor `"eng"` 硬编码修复**——`extract()` 改读全局 `ocr_lang` 设置（OnceLock 缓存 DB 连接池），不再写死英语（`src-tauri/src/extractor/pdf.rs`）

---

## 2026-08-08（目录嵌套吸收 · 添加父目录复用子目录索引）

- **添加已含子目录的父目录被拒绝**：`add_dir(/A)` 因已索引 `/A/B` 报"此目录包含已索引目录"。现改为**吸收**——添加父目录时，把被包含子目录的 `file_tracking` 记录重根到父目录（`path` 加子目录相对前缀如 `B/`、`dir_id` 指向父、`indexed` 重置），删除子目录配置并停 watcher；Tantivy 旧文档按 `dir_id` 删除，父目录重扫时通过 MD5/content 去重**复用已提取内容**、只重建索引文档，不重复提取（`src-tauri/src/commands/dirs.rs`、`src-tauri/src/db/tracker.rs`、`src-tauri/src/search/indexer.rs`、`src-tauri/src/indexer.rs`）
- **新增**：`tracker::absorb_subdir`（事务内批量迁移，保留 md5）、`Indexer::delete_by_dir` + `IndexerService::delete_dir`（按 dir_id 删 Tantivy 文档）
- **吸收后自动全量扫描**：`add_dir` 吸收子目录后后台触发父目录 `full_scan`（`is_scanning` 防重入），Tantivy 文档立即以新路径重建，无需手动扫描；内容经 MD5 去重复用（`src-tauri/src/commands/dirs.rs`）
- **测试**：`test_absorb_subdir_reroots_paths_and_dir_id` 覆盖路径重根/目录切换/md5 保留/indexed 重置；93 单元 + 3 smoke + 9 集成通过

---

## 2026-08-08（长音频内存爆炸修复 · OOM）

- **长音频索引导致 OOM 崩溃（18.7GB）**：用户索引 34 分钟录音（2071s）时 RSS 飙升至 18.7GB，进程被系统杀掉。根因——VAD 模型缺失时 `recognize_segments()` 返回 `None`，代码回退 `transcribe(rec, &samples)` **把整段长音频一次性喂给 FunASR-Nano（LLM 架构）**，解码内存失控
- **修复**：`recognize_segments()` 改为**永远返回有界分段**——Silero VAD 可用时用 VAD（≤30s），不可用时**固定 28s 硬切回退**；彻底移除"整段喂入"路径。实测 2071s 音频 RSS 峰值从 18.7GB → 1.5GB，转写成功（483s）（`src-tauri/src/extractor/audio.rs`）
- **VAD 模型补齐**：向各数据目录 `models/funasr/` 复制 `silero_vad.onnx`（628KB）——此前多个数据目录缺失导致走硬切路径；`install_funasr` 已有 VAD 下载逻辑，此为先验修复

---

## 2026-08-08（长音频转写修复 · Silero VAD 分段）

- **长音频（>30s）返回「无识别结果」**：用户 209 秒 mp3 转写为空。根因——FunASR-Nano 是 LLM 架构（Qwen-0.6B），训练输入约 30s 窗口，整段 209s 一次性喂入超出长度上限，模型静默返回空。旧 Python 方案有 `fsmn-vad` + `max_single_segment_time=30000` 分段，sherpa-onnx 迁移时漏掉了
- **修复**：`extract_audio` 增加 **Silero VAD 分段**（`VoiceActivityDetector`，`max_speech_duration=30s` 对齐官方）——512-sample 窗口滑动喂入 → `flush` 取段 → 每段独立识别 → 文本拼接；VAD 模型缺失时自动回退单次识别（短音频不受影响）（`src-tauri/src/extractor/audio.rs`）
- **VAD 模型随安装下载**：`install_funasr` 在校验主模型后追加下载 `silero_vad.onnx`（628KB，独立小文件非归档内），同样走 GitHub→ModelScope 镜像回退 + 100KB 有效性校验；下载失败不阻塞（只影响长音频分段）（`src-tauri/src/commands/funasr.rs`）
- **验证**：209s 真实通话录音转写成功（分段识别合并，37s 处理）；新增 `audio_extractor_long_audio` 冒烟测试（`tests/funasr_smoke.rs`）

---

## 2026-08-08（文档：模型存储目录澄清）

- **打包版检测不到已下载模型**：用户模型在项目 `src-tauri/models/funasr/`（dev 位置），但双击运行的 .app 只认数据目录 `~/Library/Application Support/link-searcher/models/funasr/`（与 db/索引同源）。文档补清两处：USER_MANUAL 8.1 存储位置列出 `models/funasr/` 并加 dev vs 打包版差异说明；`models/funasr/README.md` 修正手动下载的错误路径 `com.linksearcher.app`（应为 `link-searcher`），并注明归档顶层目录需平铺

---

## 2026-08-08（FunASR 模型下载镜像自动回退）

- **GitHub 下载慢/失败时模型装不上**：`install_funasr` 之前只认 `LINK_SEARCHER_FUNASR_MIRROR=modelscope` 环境变量，普通用户不知道、下载 842MB 从 GitHub 直连可能超时。改为**双源自动回退**：默认 GitHub 先行，失败/超时（每源 180s 上限）自动切 ModelScope 镜像重试；环境变量仍可强制镜像专用（`src-tauri/src/commands/funasr.rs`）
- **下载超时保护**：`download()` 每源 180s 上限，连接建立后无进展即中止并切源，安装器不再无限期挂起
- 新增 `download_sources_modes` 单元测试覆盖默认回退/强制镜像两种模式

---

## 2026-08-08（ffmpeg 自动检测）

- **音频提取报 `ffmpeg not available`**：`Command::new("ffmpeg")` 只查 PATH，Tauri 运行时（不继承终端 PATH）找不到 Homebrew 的 ffmpeg → mp3 等音频提取全部失败、静默降级为纯文本回退（结果为空）。新增 `find_ffmpeg_binary()` 多候选探测：PATH → `ffmpeg-bin/`（dev 相对）→ 可执行文件旁 → `/opt/homebrew/bin` 等常用前缀，`OnceLock` 缓存；未找到时返回明确安装指引（`src-tauri/src/extractor/audio.rs`）
- **build.rs 打包 ffmpeg**：像 poppler 一样将 `ffmpeg` 拷贝进 `ffmpeg-bin/`，app bundle 自带解码器（`src-tauri/build.rs`）
- **依赖检测 + 启动日志**：`check_dependencies()` 新增「FFmpeg (音频解码)」条目（含 brew 安装指引）；`[STARTUP]` 日志增加 `ffmpeg=OK/MISSING`（`src-tauri/src/commands/tesseract.rs`、`src-tauri/src/lib.rs`）

---

## 2026-08-08（文件类型页 · 扫描过但不支持）

- **扫描静默丢弃的问题文件可见化**：此前不支持的扩展名被 `extension_allowed` 过滤后无声消失，用户看不到「为什么这些文件搜索不到」。现扫描器在全量/增量/启动扫描 3 处过滤点记录被跳过文件的扩展名计数（`unsupported_ext_stats` 表，`bump_unsupported_ext`），文件类型页新增「扫描过但不支持」区块（`src-tauri/src/scanner/mod.rs`、`src-tauri/src/db/mod.rs`、`src-tauri/src/db/tracker.rs`）
- **新命令 `get_unsupported_ext_stats`**：返回扩展名 + 出现次数 + 是否可救（wps/et/dps 等 LibreOffice 可救格式标记缺依赖并给安装指引，其余标「暂无提取器支持」）（`src-tauri/src/commands/tesseract.rs`、`src-tauri/src/lib.rs`）
- **前端**：FileTypes 页在支持列表下方展示不支持扩展名，区分「缺少依赖」（琥珀色）与「不支持」（红色），含文件计数与提示文案；i18n 新增 4 键（`src/pages/FileTypes.tsx`、`src/i18n/zh.ts`、`src/i18n/en.ts`）
- **scan-run 重置**：`unsupported_ext_stats` 由累计制改为快照制——每个扫描函数遍历前按 `dir_id` 清空（`reset_unsupported_ext`），扫描完成后即该目录的当前磁盘状态；多目录独立、取消扫描后下次自愈（`src-tauri/src/scanner/mod.rs`、`src-tauri/src/db/tracker.rs`）

---

## 2026-08-08（FunASR 零 Python 化 · sherpa-onnx）

- **彻底移除 Python venv 依赖**：音频 STT 从「venv + torch + funasr + infer.py」迁移为 sherpa-onnx Rust crate（1.13.4，Apache-2.0）进程内推理 `Fun-ASR-Nano-2512` ONNX int8 模型。`infer.py`、`install_funasr` 的 venv/pip 逻辑全部删除，`data_dir/models/funasr/.venv`（约 1.3GB）已清理（`src-tauri/Cargo.toml`、`src-tauri/src/extractor/audio.rs`、`src-tauri/src/commands/funasr.rs`）
- **模型改为下载制（方案 B）**：`install_funasr` 命令改为「下载 + 解压 sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2（~850MB，GitHub；`LINK_SEARCHER_FUNASR_MIRROR=modelscope` 走国内镜像）→ 校验 4 个必需文件」，后台线程 + 每 5% 进度日志 + 完成后 emit `funasr-install-done` 事件，前端交互零改动（`src-tauri/src/commands/funasr.rs`）
- **识别器全局复用**：`OfflineRecognizer` 存 `OnceLock` 单例，避免每文件重复加载 ~950M int8 权重；解码参数对齐官方配置（`greedy_search`、`temperature=1e-6`、`top_p=0.8`、`user_prompt="语音转写："`、`max_new_tokens=512`）（`src-tauri/src/extractor/audio.rs`）
- **就绪检查更新**：`funasr_venv_ready()` → `funasr_model_ready()`，依赖检测/启动日志/设置页指引全部指向模型下载而非 venv；build.rs 的 `HAS_ASR_MODELS` 改查新 int8 布局（`src-tauri/src/commands/tesseract.rs`、`src-tauri/src/lib.rs`、`src-tauri/build.rs`）
- **文档与文案**：`models/funasr/README.md` 重写为下载制；README/USER_MANUAL i18n 文案从「2GB venv」改为「850MB 模型下载」；**注意：不再输出 `[Speaker X]` 说话人分离**（sherpa-onnx 接口仅返回整段文本）
- **构建说明**：sherpa-onnx-sys 首次构建需联网下载静态链接库（18M，缓存于 `target/sherpa-onnx-prebuilt/`）；本机有代理时如遇 `UnknownIssuer` TLS 错误，手动下载归档并设 `SHERPA_ONNX_ARCHIVE_DIR`

---

## 2026-08-08（FunASR 启动依赖检测 + 自动安装）

- **build 后 FunASR venv 定位失败**：venv（约 2.3GB）不进包，dev 仓库内的 venv 在 build 后 cwd 不定的情况下找不到。`funasr_candidates()` 新增「从可执行文件向上逐级找 `src-tauri/models/funasr`」，开发机上 build 产物零拷贝直接复用仓库 venv（`src-tauri/src/extractor/audio.rs`）
- **FunASR 纳入依赖检测**：`check_dependencies()` 新增「FunASR (音频转写)」条目，设置页自动显示 ✓/✗ + 安装指引；启动日志 `[STARTUP] PaddleOCR=.. LibreOffice=.. pdftoppm=.. FunASR=OK/MISSING`（`src-tauri/src/commands/tesseract.rs`、`src-tauri/src/lib.rs`）
- **一键自动安装**：新增 `install_funasr` 命令——`python3 -m venv` + `pip install funasr torch torchaudio` 到 `data_dir/models/funasr`，后台线程执行不阻塞 UI，pip 输出逐行写日志，完成 emit `funasr-install-done`，`AtomicBool` 防重入；无 python3 时报错提示（`src-tauri/src/commands/funasr.rs`，新文件）
- **启动交互**：App 挂载时查依赖，FunASR 缺失 → `ask()` 确认弹窗（约 2GB）→ 确认即安装，拒绝本次会话不再打扰（`sessionStorage`）；设置页依赖卡内置「立即安装」按钮（`src/App.tsx`、`src/pages/Settings.tsx`）
- **i18n**：新增 `confirm_install_funasr` / `funasr_install_prompt` / `install_now` / `not_now` / `installing`（zh/en）

---

## 2026-08-08（索引日志过滤 + ASR 候选链 + 清空确认）

- **单文件索引日志混入无关行**：`get_logs(file_id)` 上次修复返回最后一次 `开始:` 之后**全部**行，并行批次交错后混入其他文件/启动/tantivy/`[BROWSE]` 日志。改为从最后一次 `开始:` 起只保留含 `[fid]` 标记的行（开始/提取/完成均带 `[INDEX] [id]`）（`src-tauri/src/commands/logs.rs`）。日志中残留的 `[BROWSE]` 行来自修复前旧版本启动的历史记录，append 保留属预期
- **build 后 FunASR 仍找不到 venv**：探测链少了 data_dir 候选。抽出 `funasr_candidates()`，新增 `data_dir/models/funasr`（`crate::config::load_config()`）为第 4 候选；`[ASR 环境未安装]` 错误信息列出全部已探测路径，便于定位（`src-tauri/src/extractor/audio.rs`）
- **清空日志误触**：LogViewer 清空按钮直接截断日志无确认。新增 `confirm_clear_logs` i18n key（zh/en），点击后 `ask()` 二次确认（kind=warning）（`src/pages/LogViewer.tsx`、`src/i18n/zh.ts`、`src/i18n/en.ts`）

---

## 2026-08-08（7 项日志/浏览/ASR 缺陷修复）

- **[BROWSE] 无关日志**：`files.rs` 每次浏览查询打印 `[BROWSE] filter/sort/page` 参数，与索引内容无关，已删除（`src-tauri/src/commands/files.rs`）
- **build 后 mp3 无法转录**：`audio.rs` 的 `MODEL_DIR="models/funasr"` 与 `infer.py` 均为相对路径，dev 时 cwd=src-tauri 可见 `.venv`，build 后 cwd 不定导致 `model_ready()` 失败。改为 `funasr_dir()` 多候选探测（`LINK_SEARCHER_FUNASR_DIR` 环境变量 → 可执行文件旁 → 当前目录）+ 绝对路径调用（`src-tauri/src/extractor/audio.rs`）
- **日志每次启动被清空**：`File::create` 每次启动 truncate `app.log`。改为 `OpenOptions::append` 跨启动保留 + 超 100MB 轮转 `app.log.1`（`src-tauri/src/lib.rs`）
- **清空日志失效**：`clear_logs` 只写 UTC `Z` 格式 marker，而日志时间戳已是本地 `+08:00`，字符串比较下所有日志行都大于 marker，过滤失效。改为直接截断 `app.log`，`get_logs` 去掉 marker 逻辑（`src-tauri/src/commands/logs.rs`）
- **查看索引日志显示全部历史**：Browse 的索引日志按 `[fileId]` 过滤全部历史行。`get_logs` 新增可选 `file_id` 参数，只返回最后一次 `[INDEX] [id] 开始:` 之后的完整索引过程（`logs.rs`、`src/pages/Browse.tsx`）
- **浏览筛选切 tab 后丢失**：新增 `usePersistentState` hook（localStorage 持久化），Browse 的 filter/ext/search/sort/order 跨 tab 保留（`src/hooks/usePersistentState.ts`、`src/pages/Browse.tsx`）
- **多选右键菜单错乱**：普通单击清空 selectedIds 导致多选实际为单选；右键菜单在 size≤1 时显示打开/Finder 只对第一个文件生效。修复：单击改为单选当前项、右键未选中行先单选；**多选时右键菜单只显示「手动索引」，打开/Finder/索引日志仅单选显示**（`src/pages/Browse.tsx`）

---

## 2026-08-08（日志时间戳改本地时区）

- **日志时间戳显示 UTC 而非本地时间**：`env_logger` 默认 `format_timestamp_secs()` 输出 UTC（如 `2026-08-07T22:37:06Z`），用户看到的时间与本地相差 8 小时。改为自定义 `format` 闭包用 `chrono::Local` 输出本地时区（`2026-08-08T06:37:06+08:00`），保持原 `[ts LEVEL module] msg` 布局（`src-tauri/src/lib.rs`；`chrono` 已在依赖中，零新增）

---

## 2026-08-08（修复 tauri build 失败：清两处 TS 死代码）

- **tauri build 失败（tsc 拦截）**：`tsc -b` 报 2 个 TS 错误导致 `npm run tauri build` 终止，而 `tauri dev` 用 esbuild 转译不做类型检查故不受影响。① `Browse.tsx` `handleReindex` 从未被挂载（右键菜单只挂 `viewIndexLog`，批量重索引用裸 `reindexFile`）——TS6133 未使用；② `SearchPage.tsx` 给 `<SearchBar>` 传 `history` prop，但 `SearchBar` 已自管 history，props 无此项——TS2322。修复：删 `Browse.tsx` 的 `handleReindex` 及孤儿 `ask` import；删 `SearchPage.tsx` 的 history state/effect/import/传参，将"query 变化后刷新历史"并入 `SearchBar` 自身 effect（`src/pages/Browse.tsx`、`src/pages/SearchPage.tsx`、`src/components/SearchBar.tsx`）

---

## 2026-08-08（Phase 1 索引进度静默丢失 + 浏览筛选补音频格式）

- **Phase 1 索引进度不可见（再次出现）**：`mark_extracted` 用 `let _` 吞错误，SQLite 并行写冲突（Rayon par_iter）时静默失败，文件停在 `indexed=0`，UI 显示已索引=0 且浏览页无法筛选已索引文件。改为失败时打 WARN 日志 + 重试一次（`src-tauri/src/indexer.rs`）。此问题已加入 AGENTS.md「反复出现的问题清单」，禁止再次出现
- **浏览筛选缺音频格式**：Browse 页文件类型下拉框补充 mp3/wav/m4a/aac/flac/ogg（与 `extractor/audio.rs` 支持的音频格式对齐）（`src/pages/Browse.tsx`）

---

## 2026-08-08（音频完整时长识别：移除 -t 60 截断）

- **音频只识别前 60 秒（回归修复）**：`audio.rs` 的 ffmpeg 解码参数带 `-t 60`，34 分钟的庭审录音只转写开头 60 秒（470 字符）。移除该参数，处理完整音频。验证：3.5 分钟文件 441→1518 字符并正确识别吴语（"扣脱来塞勿啦"等），34.5 分钟庭审完整转写 13713 字符（含"今日庭审到此结束"结尾）（`src-tauri/src/extractor/audio.rs`）
- **说明**：`language="中文"` 已隐含自动方言识别（Fun-ASR-Nano-2512 支持 7 大吴粤闽客赣湘晋方言 + 26 地区口音），无需额外参数；之前"吴语没识别"实为 60s 截断导致吴语片段未进入模型

---

## 2026-08-08（ASR 环境根治：项目 venv + Python 发现逻辑加固）

- **ASR 推理 `ModuleNotFoundError: funasr` 修复**：根因是 funasr Python 包从未装入任何环境，而 `audio.rs` 硬编码调用系统 `python3`。修复：在 `src-tauri/models/funasr/` 创建项目专属 `.venv` 并安装 funasr + torch；`audio.rs` 优先使用 `.venv/bin/python`（兜底系统 python3），`model_ready` 改校验 venv 存在，报错信息指向安装文档而非裸 traceback（`src-tauri/src/extractor/audio.rs`、`models/funasr/README.md`）
- **清除死代码环境变量**：`FUNASR_TOKENIZER_DIR` 写入的 ONNX 时代路径，infer.py 从未读取，已删除
- **文档**：README.md 前提条件补音频识别依赖说明；USER_MANUAL FAQ 补 venv 安装命令
- **验证**：真实法庭录音 mp3（60s）转写成功，含说话人分离（`[Speaker 0] 上海市闵行区人民法院…`）

---

## 2026-08-08（Browse 文件类型筛选动态加载 + test_stats 回归修复）

- **浏览文件类型筛选动态加载**：新增 `get_browse_file_types` 命令，查询 `file_tracking` 实际存在的扩展名，用 `get_supported_extensions()` 过滤（仅显示可索引类型，`.exe`/`.dll` 等不可索引类型不出现），按字典序返回。Browse 页删除 20 个硬编码 `<option>`，改为 `useEffect` 挂载时动态加载渲染，URL 中失效的 `ext` 参数自动重置（`src-tauri/src/commands/search.rs`、`lib.rs`、`src/api/files.ts`、`src/pages/Browse.tsx`）
- **test_stats 回归修复**：commit `274343f` 将 `get_stats` 的 pending 定义从 `indexed IN (0,2)` 改为 `indexed=0`（failed 不再计入 pending），导致 `test_stats` 断言 `pending=1` 失效。更新断言为 `pending=0` 匹配新语义（`src-tauri/src/db/tracker.rs`）

---

## 2026-08-07（错误分类英文关键词 + 消除 parse_filename 生产 unwrap）

- **加密/损坏英文关键词识别**：`classify_error_str` 增加 `encrypted`/`corrupted`/`password` 英文检测（与中文 `损坏`/`加密` 并列）；批量索引 Phase 1 提取失败分支也调用 `classify_error_str` 并以错误类型写入 `index_errors`，不再只 `mark_failed`（`src-tauri/src/indexer.rs`）
- **消除生产环境 unwrap**：`parse_filename_prefix` 用 `OnceLock<Regex>` 静态编译正则替代 `Regex::new().unwrap()`，并去掉 `cap.get(0).unwrap()` 改用安全匹配（`src-tauri/src/search/searcher.rs`）
- **音频 STT 提取（FunASR + 说话人分离）**：新增 `extractor/audio.rs`，支持 mp3/wav/m4a/aac/flac/ogg/opus/wma 8 种音频格式索引。ffmpeg 解码后调用 FunASR-Nano（`FunAudioLLM/Fun-ASR-Nano-2512`）官方 AutoModel 推理，内置 VAD + CAM++ 说话人分离，输出 `[Speaker X] text` 格式。模型通过 ModelScope 自动下载（无需 token），支持中文、英文、日语及吴语/粤语/闽语等 7 大汉语方言。识别结果进入全文索引（`src-tauri/src/extractor/audio.rs`、`models/funasr/infer.py`）
- **热词增量计数**：每次索引文件时自动用 jieba 分词提取 ≥2 字非数字词，存入 `hotword_counts` 表。用于提升 ASR 识别精度，下次全量扫描自动重建（`src-tauri/src/db/tracker.rs`、`mod.rs`、`indexer.rs`）
- **浏览筛选扩展**：Browse 页新增 ods/odp/rtf/epub 格式选项；后端 `extract_text` 路由同步支持（`src/pages/Browse.tsx`、`extractor/mod.rs`）
- **文件类型统计增强**：`IndexStatus` 页各扩展名展示已索引/待处理/失败分项堆叠条（`commands/search.rs`、`api/search.ts`、`IndexStatus.tsx`）
- **日志清空改截断**：`clear_logs` 写时间戳标记，不再覆盖 `app.log`；`get_logs` 只返回标记之后的日志行（`commands/logs.rs`）

---

## 2026-08-05（Dock 图标根治：原生优先 + 批量转换取代 LSUIElement hack）

- **集成 AnyDoc 作为主导擎**：替换 calamine/lopdf/quick-xml + LibreOffice 多工具链，统一为 AnyDoc（`firecrawl/anydoc` v0.1.6，纯 Rust，MIT 协议）。20 种 Office 格式（doc/docx/xls/xlsx/ppt/pptx/odt/rtf/epub/csv）均优先用 anydoc，失败回退 LibreOffice；文字 PDF 用 anydoc/pdf-inspector 优先，解决 Quartz/CFF PDF 的 `lopdf` 空格问题。提取速度 ~5ms/文件，输出统一为 Markdown。设置页新增"文档提取引擎"段，LibreOffice 降级标注为备用引擎。旧原生提取器保留代码并标 `#[allow(dead_code)]` 待后续清理（`src-tauri/Cargo.toml`、`src-tauri/src/extractor/office/mod.rs`、`tests.rs`、`pdf.rs`、`src/pages/Settings.tsx`、`src/i18n/{zh,en}.ts`）

- **压缩包提取**：新增 `extractor/archive.rs`，支持 zip/tar/tar.gz/tgz/tar.bz2/tbz2/tar.xz/txz/gz/bz2/xz 全纯 Rust 提取。枚举条目后文本文件直接读取，Office/PDF/图片解压到临时目录走现有提取管线（含 OCR），不支持格式和加密文件跳过并标注原因。100MB/1000 文件/50MB 单文件上限防炸弹。输出用 `─── path/in/zip.pdf ───` 分隔（`src-tauri/src/extractor/archive.rs`、`mod.rs`、`Cargo.toml`、`Cargo.lock`）

- **日志清空改为截断**：`clear_logs` 不再覆盖 `app.log`（破坏 logger 句柄），改为写 `log_marker` 时间戳；`get_logs` 只返回标记之后的行（`src-tauri/src/commands/logs.rs`）

- **性能日志**：`batch_index` 新增 Phase 1/2 分阶段计时和吞吐量统计，每 100 文件或 30 秒汇报进度（文件/s、MB/s、成功/失败数）。`archive.rs` 新增压缩包提取耗时和条目数统计（`src-tauri/src/indexer.rs`、`src-tauri/src/extractor/archive.rs`）

- **`is_watermark_text` 对中文文本误判**：原算法用字符集 Jaccard 相似度判断，但中文常用字仅两三千，任意两页的字符集重叠度天然高，导致 182 页财报被误判为水印，触发 OCR 回退。修复：改为归一化前缀精确比较 — 取每页前 300 字符，剔除 hex 长串（验证码/UUID）、日期、URL、空白后逐页对比，>80% 连续页归一化前缀相同时才判定水印。附带：`ocr_pdf_via_pdfimages` 结束时检查有效 OCR 页数，少于总页数一半时返回 Err，回退到 pdftoppm，防止 182 页文字 PDF 因仅 1 页有嵌入图而被截断（`src-tauri/src/extractor/pdf.rs`）

**根因诊断**（受控实验证伪三种压制方案）：
- `SAL_USE_VCLPLUGIN=svp`：不阻止 soffice 注册前台 app（`lsappinfo list` 仍出现 ASN）
- `LSUIElement=true` + `lsregister -f` 强刷缓存：仍注册前台 app（直接 exec 二进制不读 LSUIElement）
- `DYLD_INSERT_LIBRARIES` 注入 dylib：adhoc 签名不够，dyld 直接剥掉（marker 文件未创建）
→ **结论：直接执行 LibreOffice 二进制时，其启动代码必定把自己注册为前台应用，外部手段全部无效。唯一方案是减少进程启动次数。**

**修复方案**：

- **现代格式原生优先**：`.docx`→`extract_docx`、`.xlsx/.xls`→`extract_xlsx`（calamine 原生支持 `.xls`）、`.pptx`→`extract_pptx`，全部原生解析优先；仅原生解析失败或返回空时才回退 LibreOffice。此前连现代 OOXML 格式都先调 LO→ 导致大量不必要的 soffice 进程启动→Dock 图标泛滥。`.xls` 此前被路由到 LO-only 分支，calamine 的 xls 支持路径处于休眠状态——现已激活（`src-tauri/src/extractor/office/mod.rs`）

- **旧格式批量转换**：新增 `LoBatcher`——请求合并调度器。Rayon `par_iter` 并行提交的 `.doc/.ppt` 提取请求进入全局队列，leader 线程收集聚合成批（最多 32 个/批，300ms 收集窗口），单次 `soffice --convert-to` 进程转换整批。`extract_many_via_libreoffice` 内部处理 stem 碰撞（同名输出文件覆盖→分 sub-round 转换）。Leader-election 模式保证并行 Rayon 线程不饿死且无死锁。**索引器零改动**（`src-tauri/src/extractor/office/mod.rs`）
  - 附带疗效：serialize 了 LibreOffice 调用，根治旧日志里的 `DeploymentException` 并发崩溃
  - 超时按批大小缩放：30s + 15s×N，上限 600s

- **LO 路径缓存**：`lo_binary()` 用 `OnceLock` 缓存进程内第一次 `check_binary` 结果，后续每文件不再 spawn `soffice --version`（每个 `--version` 本身也是一次 Dock 图标）
  - 保留原 `is_libreoffice_available()` 用于设置/面板 UI（不缓存，支持用户换路径后即时检测）

- **移除 LoBackgroundGuard::enter 三处调用**：对 `lib.rs` 启动扫描、`index.rs` 的 `trigger_scan` 和 `rebuild_index` 三处 `LoBackgroundGuard::enter()` 已证实无效且会修改用户已签名 LibreOffice 的 Info.plist（破坏签名 + LaunchServices 缓存不认 → 白改）。保留 `ensure_lo_background_mode`（= recover）在启动时清理残余 LSUIElement（`src-tauri/src/lib.rs`、`src-tauri/src/commands/index.rs`）

- **修复预存测试竞态**：`test_index_file_creates_document` 与 `test_delete_file` 并行时共享 `tmp_file("test.txt")` 导致文件覆盖。改名 `test_create.txt` 避免冲突（`src-tauri/src/indexer.rs`）

- **macOS `open -gj` 彻底消除遗留 Dock 图标**：批量转换中残留的每次 soffice 进程启动仍会产生一次 Dock 闪现。改为通过 `open -gj -b org.libreoffice.script --args ...` 启动——LaunchServices 以 hidden 模式运行（`lsappinfo` 显示 `(hidden)`，等效 LSUIElement），彻底无 Dock 图标。PID 通过 `pgrep` 差集追踪，超时时用 PID 精确 kill。`open -gj` 失败时自动回退直接 exec（`src-tauri/src/extractor/office/mod.rs`）

- **批量转换批大小可配置**：原硬编码 32 文件/批改为用户设置项 `lo_batch_size`（1–100）。全局 `AtomicUsize` 零锁读取，保存设置后即时生效无需重启。新增前端 `NumberField` 控件 + i18n 文案（zh/en）（`src-tauri/src/extractor/office/mod.rs`、`lib.rs`、`commands/settings.rs`、`db/mod.rs`、`Settings.tsx`、`zh.ts`、`en.ts`）

- **Windows OCR 实现**：新增 `windows_ocr.rs` 模块，使用 Windows 10+ 原生 `Windows.Media.Ocr.OcrEngine`，同步 `.get()` 阻塞模式。与 Apple Vision 镜像设计：同签名、同语言映射、同 `_with_regions` 诊断接口。非 Windows 平台保留错误提示桩（`src-tauri/src/extractor/windows_ocr.rs`）
- **依赖**：新增 `windows` 0.61，target-conditional（`cfg(target_os = "windows")`），macOS/Linux 编译零影响（`Cargo.toml`）
- **引擎分发**：`ocr.rs` 两处 `WindowsOcr` 分支从 PaddleOCR 桩替换为 `windows_ocr::recognize_from_path` / `_with_regions`；`mod.rs` 注册模块（`src-tauri/src/extractor/ocr.rs`、`mod.rs`）

- **PDF 视觉水印 OCR 污染**：扫描件 PDF 中，水印文字被 `pdftoppm` 渲染到页面图像上，导致 OCR 回退后水印仍被读出。修复：检测到文字层含水印后，优先使用 `pdfimages` 提取原始图像层（不解码文字层/注解层叠加），再对每页最大图像做 OCR，从根本上避免水印污染。`pdfimages` 不可用时回退原有 `pdftoppm` 路径（`src-tauri/src/extractor/pdf.rs`）

- **Tauri GUI PATH 不含 Homebrew → `pdfimages`/`pdftoppm` 找不到**：Tauri macOS 应用 PATH 不包含 `/opt/homebrew/bin`，导致 `Command::new` 找不到 poppler 二进制，OCR 回退完全跳过。修复：新增 `find_poppler_binary` 按 `/opt/homebrew/bin` → `/usr/local/bin` → `/usr/bin` 回退查找，结果用 `OnceLock` 缓存；`is_*_available` 和所有 OCR 函数均改为使用缓存的绝对路径。附带：`[WATCHER] file Modify` 日志对排除文件（`.DS_Store` 等）降级为 `debug!` 级别（`src-tauri/src/extractor/pdf.rs`、`src-tauri/src/lib.rs`）

- **`try_ocr_fallback` pdfimages 短文本未回退**：部分 PDF 由多图层合成（JPEG 底图 + stencil 文字层叠加），`pdfimages` 提取到 JPEG 底图后 OCR 仅返回几个字符（如"签署页：上海線"），`!is_empty()` 判定成功导致不触发 pdftoppm 回退。修复：pdfimages OCR 成功阈值改为 100 字符以上（`src-tauri/src/extractor/pdf.rs`）

- **`lopdf` 严格解析导致部分 PDF 无法提取**：`lopdf` 对语法错误 PDF（stream `Bad Length`、缺 `endstream`）直接 `failed to load PDF`，而 poppler 工具可正常读取。修复：`extract_with_lang` 中 `lopdf` 失败后依次尝试 `pdftotext` 提取文字层（含水印检测） → `pdfimages` 图像层 OCR → `pdftoppm` 全页 OCR，确保数字生成 PDF 和扫描件均能提取。附加：`ocr_pdf_via_pdfimages` 页数获取改用 `pdfinfo` 优先、`lopdf` 兜底；图片面积 <100k 像素时跳过该页（过滤 logo/二维码等嵌入素材而非页面截图）；提取 `try_ocr_fallback` 统一 OCR 回退链（`src-tauri/src/extractor/pdf.rs`）

---

## 2026-08-03（消除残留硬编码：图片 OCR + OCR 回退接入引擎分发）

- **`ocr_image` 便利函数硬编码 PaddleOCR**：图片文件索引和 indexer 短文本 OCR 回退均通过 `ocr_image` 走 PaddleOCR，忽略引擎设置。修复：`ocr_image` 新增 `engine: Option<OcrEngineType>` 参数并通过 `ocr_image_with_engine` 分发；`mod.rs` 图片分支改用 `ocr_image_with_engine`（不再依赖 `ImageExtractor.extract`）；`indexer.rs` 短文本回退传入 `ocr_engine`（`src-tauri/src/extractor/ocr.rs`、`mod.rs`、`indexer.rs`、`tests/integration.rs`）

---

## 2026-08-03（PDF OCR 接入引擎分发：不再硬编码 PaddleOCR）

- **PDF OCR 始终走 PaddleOCR，忽略用户引擎选择**：`pdf.rs` 的 `ocr_pdf_via_pdftoppm` 硬编码 `paddleocr::recognize_from_path_with_regions`，完全绕过 `ocr.rs` 的引擎分发。即使设置页选了 Apple Vision，PDF OCR 仍跑 PaddleOCR → 用户无法感知 Vision 加速。修复：`ocr_pdf_via_pdftoppm`/`extract_with_lang`/`extract_text` 新增 `engine: Option<OcrEngineType>` 参数；`indexer.rs` 从 `app_settings.ocr_engine` 读取配置传入；`ocr.rs` 新增 `ocr_image_with_regions` 调度函数（PaddleOCR/AppleVision/Tesseract 三路）；`apple_vision.rs` 新增 `recognize_from_path_with_regions` 输出 region 数（`src-tauri/src/extractor/pdf.rs`、`mod.rs`、`ocr.rs`、`apple_vision.rs`、`indexer.rs`、`test_pdf_ocr.rs`）

---

## 2026-08-03（Apple Vision OCR 引擎：macOS 原生 OCR，ANE 推理）

- **Apple Vision OCR 实现**：新增 `apple_vision.rs` 模块，使用 macOS 10.15+ 原生 `VNRecognizeTextRequest`（Accurate 级别），运行在 ANE（Neural Engine）上独立于 CPU。与 tract 单线程 CPU 推理相比，预期单区域 0.05-0.2s（当前 1.5-5s），无需手动引擎池管理。实现参考 thuki（Tauri 2 + Vision OCR）和 Pointra-Pub（缓存请求 + 启动预热）生产级项目，`performRequests_error:` 同步调用模式，`autoreleasepool` 包裹防 ObjC 临时对象泄漏（`src-tauri/src/extractor/apple_vision.rs`）
- **依赖**：新增 `objc2` 0.6、`objc2-foundation` 0.3、`objc2-vision` 0.3（`Cargo.toml`）
- **引擎分发**：`ocr.rs` 的 `AppleVision` 分支从 PaddleOCR 桩替换为 `apple_vision::recognize_from_path`；`mod.rs` 注册模块（`src-tauri/src/extractor/ocr.rs`、`mod.rs`）
- **启动预热**：`lib.rs` 启动时后台线程用 64×64 空白图跑一次 Vision，预加载 CoreML/ANE 模型，消除首次调用 1-3s 延迟（Pointra-Pub 模式）（`src-tauri/src/lib.rs`）
- **语言映射**：`eng→en-US`、`chi_sim→zh-Hans`、`jpn→ja-JP`、`kor→ko-KR`（Fast 模式不支持中日韩，固定 Accurate）

---

## 2026-08-03（引擎池诊断：曝光 set_pool_size 静默失败 + macOS P-core 绑定）

- **`set_pool_size` 从未生效（池始终=4）**：`lib.rs` 启动时读 `ocr_concurrent` 的 if-let 链**三层静默吞错**（`db_pool.get()`/`query_row`/`parse` 任意失败均无日志）。修复：改为 `match` 逐层 `log::warn!`，成功后 `log::info!` 记录池大小（`lib.rs`）
- **E-core 拖慢实锤**：9 页 OCR 实测 5 页落在 E-core（效率核），最慢页 `1.76s/区域 → 5.35s/区域`（3× 差距），导致池=4 实际加速比仅 1.48×（预期 3×+）。修复：`paddleocr.rs` 新增 `pthread_set_qos_class_self_np(USER_INTERACTIVE)`，在每次 OCR 推理前向 macOS 调度器声明需要性能核偏好（`paddleocr.rs`）。池构建时追加 `log::info!` 打印引擎数
- 涉及文件：`src-tauri/src/extractor/paddleocr.rs`、`src-tauri/src/lib.rs`

---

## 2026-08-03（设置保存失败：前端回传整个 settings 对象触发白名单拒绝）

- **修改任何设置都报 "Failed to save setting"**：根因是 `Settings.tsx` 的 `handleFieldChange` 通过 `updateSettings({ ...settings, [key]: value })` 把**整个 settings 对象**发回后端，而该对象来自 `get_settings` 返回的 DB **所有行**，包含非白名单键（`theme`、`onboarding_done`、`last_scan_*`、`schema_version` 等）。后端 `update_settings` 白名单校验遇到第一个非法键即整体拒绝 → 改任何设置都失败。修复：只发送被修改的单键 `updateSettings({ [key]: value })`（`src/pages/Settings.tsx`）。附带确认 `Settings.tsx` 用到的 9 个键全部在后端 `ALLOWED_KEYS` 白名单内；`onboarding_done` 写入失败被 `.catch` 静默吞掉且已有 localStorage 兜底，无功能影响

---

## 2026-08-03（PDF OCR 每页诊断日志 + 池大小暴露）

- **PDF OCR 性能疑点定位**：9 页扫描件 OCR 耗时 319s（平均 35s/页），接近串行耗时（9×35s），与 2 引擎并行预期（~175s）不符。原因待定：可能是引擎被 Rayon 调度到 E-core（效率核）拖慢，或池实际大小非预期。此前日志无池大小与每页耗时，无法区分。修复：`paddleocr.rs` 新增 `active_pool_size()`（惰性池未构建时为 0）和 `recognize_from_path_with_regions()`（返回文本 + 区域数）；`pdf.rs` OCR 循环每页记录耗时/区域数/字符数，起始日志打印池大小（`src-tauri/src/extractor/paddleocr.rs`、`src-tauri/src/extractor/pdf.rs`）

---

## 2026-08-03（PDF OCR 提速：引擎池 + 多页并行 + 渲染 DPI 300→200）

- **PDF OCR 每页 30-60 秒过慢（实测验证）**：用 `pure_onnx_ocr::run_with_metrics_from_path` 对 200 DPI 单页实测：总 54.4s = 检测推理 16.5s（30%）+ 识别推理 34.9s（64%）+ 缩放/后处理 ~3s。识别批张量恒为 `[N,3,48,320]`（`RecPreProcessor` 满宽 320 分配），tract 无 intra-op 并行且 batch 线性展开，故「分块降 3-5 倍」对 tract 不成立。确定性的收益来自两处：
  - **全局单引擎 Mutex 串行化**：`paddleocr.rs` 原为 `OnceLock<SendEngine>`，所有 OCR 调用（多页 PDF 逐页、Rayon batch_index 多文件）在同一个 Mutex 上排队，多核闲置。修复：改为 `EnginePool`（N = min(可用核数, 4)，每个引擎独立 Mutex + round-robin 负载均衡），并发 OCR 调用分散到多核
  - **多页 PDF 逐页串行 OCR**：`pdf.rs` 原来 `loop` 逐页 `ocr_image`，N 页线性累加 54.4s/页。修复：改为 Rayon `par_iter` 并行处理全部页面，按页码顺序收集结果，多页 PDF 总耗时接近单页耗时 × 页数/核数
  - **渲染 DPI 300→200**：`-r 300` 渲染 870 万像素（A4 2480×3508），检测模型 `det_limit_side_len=960` 只消费 960px，剩余像素浪费在 Lanczos3 下采样。降到 200（1654×2339），缩放计算减少 ~2.25 倍，仍高于 960px 需求（150 仅再省 ~1s，准确率风险不值得）
  - **新增分阶段耗时诊断**：`paddleocr::recognize_with_metrics_from_path` 输出 decode/det(pre,inf,post)/rec(pre,inf,post) 各阶段秒数；`tests/test_pdf_ocr.rs` 新增 `test_ocr_bench_single_page` 实测基准
  - **修复 `extract_text` 签名变更遗漏**：`b688d1c` 将 `extract_text(path)` 改为 `extract_text(path, lang)`，但 `tests/integration.rs` 5 处调用未同步 → 编译错误。补上 `"eng"` 参数（`src-tauri/tests/integration.rs`）
  - **引擎池大小尊重 `ocr_concurrent` 设置**：新增 `paddleocr::set_pool_size(n)`（0=自动，上限 8），`lib.rs` 启动时在 DB 初始化后、health_check 前读取 `app_settings.ocr_concurrent` 注入（health_check 会惰性构建池，必须提前注入）。顺带修复前后端键名不一致：前端 `Settings.tsx` 用 `ocr_concurrency` 但后端白名单 + DB 种子均为 `ocr_concurrent` → 保存被后端以 "unknown setting key" 拒绝，该设置从未生效（`src-tauri/src/extractor/paddleocr.rs`、`src-tauri/src/lib.rs`、`src/pages/Settings.tsx`）
  - 涉及文件：`src-tauri/src/extractor/paddleocr.rs`、`src-tauri/src/extractor/pdf.rs`、`src-tauri/src/lib.rs`、`src-tauri/tests/integration.rs`、`src-tauri/tests/test_pdf_ocr.rs`、`src/pages/Settings.tsx`、`CHANGELOG.md`

---

## 2026-08-03（PDF OCR 提速：渲染 DPI 300→200）

- **PDF OCR 每页 30-60 秒过慢**：根因是 `ocr_pdf_via_pdftoppm` 硬编码 `-r 300` 渲染 870 万像素大图（A4 2480×3508），而检测模型 `det_limit_side_len=960` 只消费 960px，剩余像素全浪费在 Lanczos3 下采样上。修复：渲染 DPI 降到 200（A4 1654×2339），缩放计算量减少约 2.25 倍，仍高于检测模型所需 960px。实测各阶段耗时验证见 `tests/test_pdf_ocr.rs`（`src-tauri/src/extractor/pdf.rs`）

---

## 2026-08-03（浏览页扩展名排序修复 + 已索引文件手动重索引确认 + macOS LibreOffice 路径探测）

- **浏览页分页「翻到第 N 页后空白」根因修复**：前端传 `page_size`（snake_case）但 Tauri 2 `#[tauri::command]` 默认将 Rust 参数名 `page_size` 转为前端 `pageSize`（camelCase）→ 参数丢失，后端走默认 `ps=50`，而前端按 `pageSize=20` 计算 `totalPages` → 翻页时 offset 与实际页面对不上（例：第 109 页前端 offset=2160，后端 offset=5400>5358→零行）。修复：前端 `listFilesDb` 参数名 `pageSize` 对齐 Tauri 自动驼峰命名（`src/api/files.ts`、`src/pages/Browse.tsx`）
- **浏览页「扩展名 A-Z」排序失效 + FileTypes 类型统计为空**：根因是 `file_tracking` 表从未存在 `file_ext` 列，但 `list_files_db` 的 `ORDER BY file_ext` 和 `get_file_type_stats` 的 `GROUP BY file_ext` 都引用了它 → 两个查询直接报错（前端 `.catch` 静默吞掉）。修复：schema 升级到 v2，新增 `file_ext` 列 + `ensure_file_ext_column` 幂等迁移（ALTER TABLE + Rust 侧 `Path::extension()` 回填，避免目录名含点误判）；`upsert_file`/`update_file_path`/`migrate_paths_to_relative` 同步维护该列（`src-tauri/src/db/mod.rs`、`src-tauri/src/db/tracker.rs`）
- **已索引文件手动重索引无确认**：右键菜单「手动索引」对已索引（indexed=1）文件直接执行，可能覆盖现有索引。修复：前端 `handleReindex` 用 `ask()` 弹确认框「该文件已索引，重新索引将重新提取并覆盖现有索引数据」，确认后才执行；新增 `confirm_reindex` i18n 键。失败/待索引文件仍直接执行（`src/pages/Browse.tsx`、`src/i18n/zh.ts`、`src/i18n/en.ts`）
- **浏览页页码越界空白**：当结果集缩小时（如重扫后失败文件减少），`page` 可能超过 `totalPages`，停留在空页。修复：新增 effect 将 `page` 钳制到有效范围（`src/pages/Browse.tsx`）
- **macOS 默认 soffice 路径不可解析**：`determine_lo_binary` 先返回 config 中默认的裸 `"soffice"`，macOS GUI 应用 PATH 不含 brew 路径 → 永远找不到真路径。修复：config 默认改为空（自动探测）；`determine_lo_binary` 在 config 为默认值时跳过，按顺序探测 `/opt/homebrew/bin/soffice` → `/usr/local/bin/soffice` → `/Applications/LibreOffice.app/...`；新增 `resolved_lo_binary()`，依赖面板显示真实解析路径而非 `soffice`（`src-tauri/src/extractor/office/mod.rs`、`src-tauri/src/config.rs`、`src-tauri/src/commands/tesseract.rs`）
- **新增分页回归测试**：`tests/test_pagination.rs` 验证 507 行数据时第 11 页返回 20 行、带 ext 参数时 LIMIT/OFFSET 绑定正确（`src-tauri/tests/test_pagination.rs`）

---

## 2026-08-02（Bug 修复：macOS LibreOffice headless 调用失败）

- **扫描会话级 LibreOffice Dock 图标抑制（revert 持久写入）**：`自启动时持久注入 LSUIElement=true` 改为 `LoBackgroundGuard` RAII guard——仅在扫描会话期间临时设置，扫描完成后自动恢复。避免持久写入导致用户正常使用 LO 时无 Dock 图标。新增 crash recovery：启动时若检测到残留 LSUIElement（上次扫描崩溃），自动清除。Guard 覆盖 `trigger_scan`、`rebuild_index`、`startup_scan` 三个入口（`src-tauri/src/extractor/office/mod.rs`、`src-tauri/src/commands/index.rs`、`src-tauri/src/lib.rs`）

- **并发 soffice 共享默认 profile 锁竞争**：`batch_index` Rayon `par_iter` 并发提取 .doc/.xls/.ppt 时多个 `soffice` 进程争用同一用户 profile `.lock` → 超时/崩溃。修复：每次调用使用独立 temp profile（`-env:UserInstallation=file://{unique}` + `--norestore --nolockcheck --nofirststartwizard`）
- **超时后子进程不 kill**：`extract_via_libreoffice` 60s 超时后 orphan 进程继续持锁 → 后续调用全挂。修复：`Arc<Mutex<Child>>` 跟踪子进程，超时时 kill + wait
- **移除无用的 LSUIElement guard**：`defaults write org.libreoffice.script LSUIElement 1` 写 preferences 域不会影响 LaunchServices（只读 Info.plist）。`SAL_USE_VCLPLUGIN=svp` 已绕过 AppKit，故删除死代码（`src-tauri/src/extractor/office/mod.rs`）
- **`check_binary` 无超时**：首次运行 profile 创建可超过默认超时。改为 `spawn + try_wait` 轮询 + 15s 超时 kill（`src-tauri/src/extractor/office/mod.rs`）

- **batch_index 定期 auto-commit 持有 MutexGuard 时调用 self.commit() 导致自死锁**：`batch_index`（263 行）获取 `self.writer` Mutex 后，`guard` 存活期间调用 `self.commit()`（347 行），`commit()` 内部再次 `self.lock_writer()` 拿同一把锁 → `std::sync::Mutex` 不可重入 → 自己等自己永久卡死。修复：`self.commit()` 改为 `Indexer::commit(writer)` 直接复用已持有的 writer。同样修复 `index_file`（`src-tauri/src/indexer.rs`）

## 2026-08-02（Batch 2+3：浏览页右键菜单 + 列宽拖拽 + 页码输入 + 复制修复）

- **浏览页右键菜单**：文件行新增 `onContextMenu`，弹出菜单含 **打开**（`openFile`）、**在 Finder 中显示**（`revealInFolder`）、**手动索引**（调 `reindex_file` 后刷新列表）。移植自 `ResultList.tsx` 的右键模式，document click 自动关闭。修复 `open_file`/`reveal_in_folder` 相对路径未解析为绝对路径的 bug——DB 存相对路径，需通过 `dir_config` 拼接（`src/pages/Browse.tsx`、`src-tauri/src/commands/files.rs`）
- **新增 `reindex_file` 命令**：支持手动逐文件重索引，查 DB 记录 → 解析绝对路径 → 调用 `indexer.index_file`。已注册 invoke_handler，前端封装 `reindexFile`（`src-tauri/src/commands/index.rs`、`lib.rs`、`src/api/index.ts`）。i18n 新增 `reindex` 键
- **列宽可拖拽**：每列独立 width 状态，表头间加 `cursor-col-resize` 拖拽手柄（onMouseDown → mousemove → mouseup），最低 80px。移植自 `PreviewPanel.tsx` 的 drag resize 模式（`src/pages/Browse.tsx`）
- **页码输入框**：前后翻页按钮间插入 `go_to` 数字输入框，Enter/失焦跳转，1..totalPages 校验。移植自 `SearchPage.tsx` 模式（`src/pages/Browse.tsx`）
- **复制命令去除平台前缀**：`Settings.tsx` `filterGuide` 现在 strip 了 `"macOS:"`/`"Windows:"`/`"Linux:"` 前缀，复制的是纯命令（`src/pages/Settings.tsx`）
- **分页空页确认**：`b1ba768` 已修复 SQL 参数错位，count/data 查询共享同一 WHERE，当前 HEAD 代码正确，无需修改

- **取消扫描无效**：`cancel_scan` 标志只在 commands/index.rs 的目录边界检查，scanner 和 indexer 的 walk 循环从不读取。修复：`Scanner`/`IndexerService` 加 `cancel_scan: Arc<AtomicBool>` 字段，通过 `with_cancel()` 构造注入；三个 walk 循环（full/incremental/startup）每文件检查标志，`batch_index` Phase 1 par_iter + Phase 2 循环均检查，取消后跳过剩余文件并提交已完成部分。取消触发的文件不会标记 failed（`src-tauri/src/scanner/mod.rs`、`indexer.rs`、`lib.rs`）
- **启动/增量扫描不重试失败文件**：用户意图——失败文件仅通过手动触发（右键"手动索引"）重试，不应自动重试。原 `needs_reindex` 对 Failed(indexed=2) 返回 true 导致 startup_scan 也自动重试。修复：去掉 Failed 条件，失败文件与正常已索引文件行为一致（仅 mtime 变化时重试）。同步修复增量扫描的 mtime 门（`src-tauri/src/scanner/helpers.rs`、`mod.rs`）
- **水印扫描件 PDF 不触发 OCR**：`pdf.rs` 的"干净文本"判定仅用 >50 字符 + 水印/乱码检测，单页或页间变化水印被漏过。修复：新增 `is_repetitive()`（≥100 字符 + ≥3 行 + >60% 重复行 ratio），阈值提至 100 字符，加 `is_rep` 条件。同时修复 `indexer.rs` 的 OCR 回退对 PDF 的错误调用（`ocr_image` 不解码 PDF → 统一跳过，PDF 内已有 OCR 逻辑）（`src-tauri/src/extractor/pdf.rs`、`indexer.rs`）

- **add_dir 内部触发扫描 + 前端 triggerScan 并发导致 IndexWriter 死锁**：`add_dir` 命令内部 `spawn_blocking(incremental_scan)`（扫描 A）和前端 `useDirs.ts` 的 `triggerScan()`（扫描 B）并发执行，两个 `full_scan` 竞争同一个 Tantivy `IndexWriter` Mutex → 两者都卡在 `lock_writer()` 上，Tantivy 线程全部 idle，扫描永远不会打印"扫描完成"。修复：去掉 `add_dir` 内部的 `incremental_scan`，仅保留 watcher 启动；扫描由前端 `triggerScan()` 独占执行（已有 `compare_exchange` 并发保护）。根因使用 `sample` 命令栈分析确认（`src-tauri/src/commands/dirs.rs`）

---

- **Semgrep 静态分析集成**：新增 `.semgrep/custom.yml`（13 条自定义规则：Rust 锁中毒/panic/fs::copy/错误吞没 + TS JSON.parse/setInterval/clipboard）；叠加官方规则集 p/rust、p/typescript、p/react、p/owasp-top-ten、p/secrets；分 ERROR/WARNING/INFO 三级，ERROR 阻塞提交零容忍；`rust-unwrap-panic`/`rust-expect-panic` 排除测试目录；`rust-rwlock-read-unwrap` 在 5 处内联测试加 `nosemgrep` 注释（`.semgrep/custom.yml`、`AGENTS.md`、`src-tauri/src/indexer.rs`、`src-tauri/src/scanner/mod.rs`）
- **AGENTS.md 提交流程**：新增步骤 3.5 "Semgrep 检查 → semgrep scan --severity ERROR 零发现" + "静态分析节"三级体系说明 + 子任务禁止改规则声明（`AGENTS.md`）

---

## 2026-08-02（三项安全加固：原子迁移 + 单实例 + 交叠检测）

- **原子化迁移**：`migrate_data` 重写为 async，采用 tmp→fsync→原子 rename 模式（先拷到 `.migrate-tmp-{uuid}`，完整落盘后 `fs::rename` 到目标）。迁移期间暂停扫描+watcher，emit 进度事件（前端显示进度条）。拷贝失败则回退清理 tmp（旧目录不动）；拷贝成功+删除旧目录失败仅弹警告，迁移仍算完成。新增「目标不能是旧目录子目录」防护。`save_config` 在 `remove_dir_all` 之前执行，防止删除后写配置失败导致指向空目录（`src-tauri/src/commands/config.rs`、`src/pages/Settings.tsx`、`src/api/config.ts`）
- **单实例限制**：新增 `tauri-plugin-single-instance` v2，注册在 Builder 最前面。第二实例启动时激活已有窗口（show+set_focus）后自动退出。`--data-dir` 实例同样受限制（`src-tauri/Cargo.toml`、`src-tauri/src/lib.rs`）
- **数据目录与监控目录交叠检测**：新增 `commands/helpers.rs` `check_data_dir_overlap`（canonicalize + 组件感知 `starts_with`，带非存在路径词法回退 + macOS `/tmp`→`/private/tmp` symlink 一致化）。三个入口全部检测——`add_dir` 拒绝、`migrate_data`/`update_config` 拒绝、`--data-dir` 启动拒绝。启动时对已存在交叠仅 `log::warn` 不阻断（`src-tauri/src/commands/helpers.rs`、`dirs.rs`、`config.rs`、`main.rs`、`lib.rs`）
- **ipc_test 适配**：fixture data_dir 改为子目录，避免 TempDir 与 add_dir 目标碰撞新交叠检查（`src-tauri/tests/ipc_test.rs`）

---

## 2026-08-02（测试修复：绝对路径→相对路径重构后集成测试）

- **test_incremental_scan 查询路径格式错误**：`file_tracking` 表 `path` 字段已改为存相对路径，但 `integration.rs` 的 `test_incremental_scan` 仍用 `env.dir_path.join("<filename>")` 绝对路径查询，导致 `None.unwrap()` panic。改为传相对路径字符串（`src-tauri/tests/integration.rs`）
- **test_pdf_multiple_pages OCR 断言不稳定**：PaddleOCR（PP-OCRv5）对程序化 PDF 渲染页识别存在大小写/空格误差（"PageTwo"→"PageTWo"），原精确匹配断言导致测试失败。新增 `contains_ignore_case` 辅助函数（大小写+空白归一化后子串匹配），断言改用该函数（`src-tauri/src/extractor/pdf.rs`）
- **ipc_test init_db 签名未同步**：`init_db` 改为 `&Connection` 参数后，`ipc_test.rs` 仍按旧签名传 `db_str` 导致编译错误。改为先 `Connection::open(db_str)` 再传 `&conn`（`src-tauri/tests/ipc_test.rs`）

---

## 2026-08-02（性能测试套件）

- **新增 perf_scan.sh**：`scripts/perf_scan.sh` 提供扫描性能基准测试能力，自动清理临时数据目录、启动应用、监控 RSS 内存（每 5s），扫描完成后输出文件数、索引/DB 大小、内存峰值/均值报告（`scripts/perf_scan.sh`）；同步更新 README 测试章节（`README.md`、`CHANGELOG.md`）

---

## 2026-08-02（UX 修复：Onboarding 重复 + 路径溢出 + ESC 关闭 + 大小写 + 导出 + 加载态）

- **OnboardingWizard 反复出现**：`App.tsx` 原来只检查 settings 中的 `onboarding_done`，清空目录后 settings 可能被重置导致弹窗重现。改为优先读 `localStorage['onboarding_completed']`，关闭时同时写入 localStorage 和 settings（`src/App.tsx`）
- **Browse 路径列溢出**：`Browse.tsx` 路径 `<td>` 的 `max-w-[280px]` 改为 `max-w-[200px]`，配合已有的 `truncate` 和 `title` 属性（`src/pages/Browse.tsx`）
- **PreviewPanel 全屏无 ESC 退出**：`PreviewPanel.tsx` 新增 `useEffect` 监听 `keydown Escape`，`fullscreen` 为 true 时调用 `onClose()`（`src/components/PreviewPanel.tsx`）
- **搜索英文大小写敏感**：`useSearch.ts` `setQuery` 统一 `toLowerCase()` 后再存 state；`search.rs` 两个 `SearchParams`（搜索/导出）构造时均 `query.to_lowercase()`，Tantivy 查询完全大小写不敏感（`src/hooks/useSearch.ts`、`src-tauri/src/commands/search.rs`）
- **导出失败无具体原因**：`SearchPage.tsx` 已有 `export_failed` 带 `error` 占位符，确认无需改动（`src/pages/SearchPage.tsx`）
- **搜索中导出按钮可重复触发**：`SearchPage.tsx` 导出按钮加 `disabled={search.status === 'loading'}` 防止并发（`src/pages/SearchPage.tsx`）

---

## 2026-08-02（中危修复：栈溢出 + DB 错误致命化 + OOM 风险）

### 🟡 中危修复（MED-1 ~ MED-9）
- **MED-1 backup dir_size 无界递归**：`backup.rs` 原 `dir_size` 递归遍历深层目录可导致栈溢出。改为迭代式 breadth-first 遍历（`vec` + `while let Some(dir) = dirs.pop()`）（`src-tauri/src/commands/backup.rs`）
- **MED-2 indexer dedup DB 错误致命化**：`indexer.rs` 原 `get_content` 瞬时 DB 错误直接 `return Err`，导致该文件索引完全放弃。改为 `log::warn!` 后落入提取逻辑（`src-tauri/src/indexer.rs`）
- **MED-3 export page_size 绕过 max_results**：`search.rs` `export_search_results` 原硬编码 `page_size: 10000`，无视用户设置的最大结果数。改为读取 `app_settings.max_results`，上限钳制至 5000（`src-tauri/src/commands/search.rs`）
- **MED-4 list_files 全量加载（已废弃）**：`files.rs` `list_files` 命令不再被前端调用（仅 `list_files_db` 使用），保留代码不变，标记废弃（`src-tauri/src/commands/files.rs`）
- **MED-5 metadata 失败绕过下载检查**：`files.rs` `download_files` 原 `metadata().map(...).unwrap_or(0)` 在权限失败时静默返回 0，绕过 500MB 检查。改为区分"文件不存在"（继续处理）和"权限不足"（报错）（`src-tauri/src/commands/files.rs`）
- **MED-6 list_files_db 缺重建守卫**：`files.rs` `list_files_db` 原缺少 `is_rebuilding` 检查，索引重建期间可读到空表。补充守卫，与 `search` 命令保持一致（`src-tauri/src/commands/files.rs`）
- **MED-7 FilterPanel 类型统计静默吞错**：`FilterPanel.tsx` 原 `getFileTypeStats` 失败 `.catch(() => {})` 静默忽略。改为 `.catch(e => console.error(...))` 记录错误（`src/components/FilterPanel.tsx`）
- **MED-8 list_files_db page_size 无上限**：`files.rs` `list_files_db` 原 `page_size.max(1)` 无上界，极端参数可致 OOM。补充 `.min(1000)` 上限（`src-tauri/src/commands/files.rs`）
- **MED-9 list_dir_entries 已删除文件仍显示**：`files.rs` `list_dir_entries` 原对软删除文件仍展示状态。改为跳过（`continue`）`status='deleted'` 的记录，不纳入目录列表（`src-tauri/src/commands/files.rs`）

---

## 2026-08-01（高危后端安全修复：线程 OOM + 进程无超时 + 路径失配）

### 🔴 高危修复（HIGH-1 ~ HIGH-4）
- **HIGH-1 watcher 无限制线程 spawn**：`lib.rs` 原每个文件事件内层 `std::thread::spawn` 导致大量文件变更时 OOM。改为单线程串行处理（恢复旧行为），`handle_event` 本身很快无需独立线程（`src-tauri/src/lib.rs`）
- **HIGH-2 pdftoppm 无超时阻塞扫描**：`pdf.rs` 原用 `.status()` 无限等待大 PDF 渲染。改为 `.spawn()` + 后台线程 `recv_timeout(120s)`，超时后用 `pkill -f pdftoppm` 终止进程（`src-tauri/src/extractor/pdf.rs`）
- **HIGH-3 PaddleOCR/Tesseract 无超时锁死全局 Mutex**：`paddleocr.rs` 新增 `with_engine_timed`（后台线程 + 120s channel timeout），`recognize_from_image` 改用；`ocr.rs` Tesseract 同样改为 spawn + 后台线程 wait + 120s timeout，超时 pkill（`src-tauri/src/extractor/paddleocr.rs`、`src-tauri/src/extractor/ocr.rs`）
- **HIGH-4 Windows 路径分隔符 mismatch**：`watcher.rs` `find_matching_dir` 原用 `Path::starts_with`，DB 存 `/` 但 watcher 给 `\` 导致失配。改为两边均 normalize 为 `/` 后比较（`src-tauri/src/scanner/watcher.rs`、`src-tauri/src/scanner/helpers.rs`）

---

## 2026-08-01（错误处理与内存序修复）

### 前端
- **clipboard 复制静默吞错**：`PreviewPanel.tsx`、`ResultList.tsx` 中 `.catch(() => {})` 改为 `.catch(e => console.warn('复制失败:', e))`，失败时记录警告而非静默忽略；启动加载/i18n 等合理静默降级路径保留不变
- **navigator.platform 弃用注释**：`Settings.tsx:470` 保留 `navigator.platform` 判断（项目未引入 `@tauri-apps/plugin-os`），加注释说明弃用状态及升级路径

### 后端
- **is_scanning 原子序加强**：`commands/index.rs` 两处 `load(Ordering::Relaxed)` 改为 `Ordering::Acquire`（线程序列化读取扫描状态），`cancel_scan` 已有 `Release`/`Acquire` 无需改动；`commit_counter`/`commit_interval` 保留 `Relaxed`（纯统计无同步语义）（`src-tauri/src/commands/index.rs`）

---

## 2026-08-01（安全修复：大文件 OOM + ReDoS + unsafe impl）

### P1 级安全修复
- **P1-2 大文件无大小限制导致 OOM**：`commands/files.rs` 下载时检查 `metadata.len()`，>500MB 直接报错"文件过大，无法下载"；`scanner/mod.rs` 移位检测跳过 >10MB 文件的 MD5 计算；`extractor/text.rs` 用 `.take(10*1024*1024)` 限制纯文本提取读取上限（`src-tauri/src/commands/files.rs`、`src-tauri/src/scanner/mod.rs`、`src-tauri/src/extractor/text.rs`）
- **P1-3 unsafe impl Send/Sync 加说明**：`paddleocr.rs` 中 `SendEngine` 的 `unsafe impl Send` / `unsafe impl Sync` 注释补充说明原因（`OcrEngine` 含非 Send 内部可变性，Mutex 串行化访问保证安全），保留 unsafe impl 不可移除（`src-tauri/src/extractor/paddleocr.rs`）
- **P1-4 ReDoS 已知低风险**：`PreviewPanel.tsx` 的 `highlightText` 已有转义 + 限 20 词，加注释说明 ReDoS 风险已评估为低风险，保持现状（`src/components/PreviewPanel.tsx`）
- **P1-1 JSON.parse 已修复**：`useSearch.ts` 中 `loadFromStorage` 已有 try/catch，无需改动

---

## 2026-08-01（第七轮：WAL 一致性修复）

### 🔴 数据库备份/迁移一致性
- **trigger_backup 直接 fs::copy 活跃 WAL DB**：`backup.rs` 原用 `std::fs::copy(&state.db_path, &db_dest)` 复制活跃 SQLite 数据库，WAL 模式下源文件与 WAL/SHM 分离，产生数据撕裂。改为 `rusqlite::backup::Backup::new(&src_conn, &mut dst_conn)` + `step(-1)` 在线备份，Busy/Locked 重试 3 次（与 `restore_backup` 模式一致，`backup.rs`）
- **migrate_data 直接 fs::copy 活跃 WAL DB**：`config.rs` 迁移时同样 `fs::copy` 活跃 DB。改为相同 Backup API 模式，目标 DB 由 `Connection::open` 创建（新文件），源 DB 保持活跃，Busy/Locked 重试 3 次（`config.rs`）

---

## 2026-08-01（今日）

### 生产代码 unwrap/expect 清理
- **lib.rs 启动链 4 处 expect → ?）**：`.setup()` 闭包已返回 `Result`，将 `db::get_pool`、`db_pool.get()`、`db::init_db`、`IndexManager::open_or_create` 四处的 `.expect()` 改为 `?` 传播，启动失败时返回错误而非 panic（`src-tauri/src/lib.rs`）
- **cli.rs 3 处 expect → ?）**：`run_cli()` 返回类型改为 `Result<()>`，三处 `.expect()` 改 `.context(...)?`，`main.rs` 捕获错误并 `exit(1)` 输出到 stderr（`src-tauri/src/cli.rs`、`src-tauri/src/main.rs`）

---

## 2026-08-01（第七轮：WAL 一致性修复）
- **3-21 getDuplicates 高频触发**：`IndexStatus.tsx` 原 useEffect 依赖 `status?.total_files`，total_files 每次变化都重新调用。改为监听 `scan-completed` 事件，仅在扫描完成后调用一次（`src/pages/IndexStatus.tsx`）
- **3-23 clipboard.writeText 未 catch**：`PreviewPanel.tsx` 和 `ResultList.tsx` 的 `navigator.clipboard.writeText` 调用缺失 `.catch()`，可能未处理拒绝。添加空 catch 处理（`src/components/PreviewPanel.tsx`、`src/components/ResultList.tsx`）
- **3-26 rebuild setTimeout(1000) 硬编码等待**：`useIndexStatus.ts` 的 `rebuild` 在 `await rebuildIndex()` 后硬编码 `setTimeout(1000)`。删除该等待，由已有 5s/30s 自适应轮询刷新状态（`src/hooks/useIndexStatus.ts`）
- **3-30 a11y 基础改进**：`SearchBar.tsx` 搜索框加 `aria-label={t('search')}`，清除按钮加 `aria-label={t('clear_search')}`；`FilterPanel.tsx` 面板加 `role="region"` + `aria-label`；同时新增 `clear_search` i18n 键（`src/components/SearchBar.tsx`、`src/components/FilterPanel.tsx`、`src/i18n/en.ts`、`src/i18n/zh.ts`）
- **3-31 FilterPanel selectedSet 每次渲染重建**：`selectedSet = new Set(dirPaths)` 改为 `useMemo(() => new Set(dirPaths), [dirPaths])`；扩展名列表从 `getFileTypeStats()` 读取真实类型分布，无数据时回退到 `COMMON_EXTS`（`src/components/FilterPanel.tsx`）
- **3-32 LogViewer key=索引导致 DOM 复用错位**：`LogViewer.tsx` 日志列表原用数组索引作为 key，过滤切换时 React 错误复用 DOM 元素。改为 `logKey(line, i)` 基于行内容前 24 字符生成唯一 key（`src/pages/LogViewer.tsx`）

---

## 2026-08-01 (今日)

### R4-A 并发与状态修复（5 项）
- **3-1 PaddleOCR Mutex.lock().unwrap() 中毒 panic**：`with_engine` 内 `lock().unwrap()` 改为 `lock().unwrap_or_else(|e| e.into_inner())`，poisoned mutex 时恢复内层值而非崩溃（`src-tauri/src/extractor/paddleocr.rs`）
- **3-2 watcher 单线程串行阻塞**：事件循环中 `handle_event` 直接调用会阻塞后续事件接收。改为对每个 event 独立 `std::thread::spawn`，大文件处理不再阻塞 watcher 线程（`src-tauri/src/lib.rs`）
- **3-3 init_db 重复创建连接池**：原 `init_db` 内部调用 `get_pool` 新建独立池，与主池隔离。改为接收 `&Connection` 参数，由调用方传入主池连接（`src-tauri/src/db/mod.rs`、`src-tauri/src/lib.rs`）
- **3-4 启动 VACUUM 无条件执行**：VACUUM 持有 SQLite 独占锁，对小于 100 MiB 的 DB 是浪费。改为先 `std::fs::metadata` 检查文件大小，仅超过 100 MiB 时才执行 VACUUM（`src-tauri/src/lib.rs`）
- **3-5 PRAGMA foreign_keys 仅首个连接生效**：原 `get_pool` 在初始连接上 `execute_batch` 设置 WAL+FK，新池连接不继承。改为 r2d2 `connection_customizer` + `CustomizeConnection::on_acquire`，每个新连接自动执行 `PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;`（`src-tauri/src/db/mod.rs`）

---

## 2026-08-01 (今日)

### R4-B 后端代码质量改进
- **3-6 batch_index/index_file 去重**：抽 `extract_and_index_single` 共享函数，消除 `batch_index` Phase 1 与 `index_file` 间约 150 行重复的读文件→MD5→去重→提取逻辑（`src-tauri/src/indexer.rs`）
- **3-9 IndexedState / FileStatus 枚举**：在 `tracker.rs` 新增 `IndexedState { Pending=0, Indexed=1, Failed=2 }` 和 `FileStatus { Active, Deleted }` 枚举，替换散落的字面量比较（`helpers.rs`、`scanner/mod.rs`、`commands/files.rs`）
- **3-11 SortField 枚举**：在 `searcher.rs` 新增 `SortField { Score, Date, Size, Name }` 枚举，`SearchParams.sort` 从 `String` 改为 `SortField`，`commands/search.rs` 和 `cli.rs` 同步更新（`src-tauri/src/search/searcher.rs`、`src-tauri/src/commands/search.rs`、`src-tauri/src/cli.rs`、`tests/integration.rs`）
- **3-12 日志 hash 截断修复**：原 `{hash:.8}` 对 String 是格式宽度而非截断，改为 `&hash[..8.min(hash.len())]`（`src-tauri/src/indexer.rs`）
- **3-16 settings key 白名单**：`update_settings` 新增 `ALLOWED_KEYS` 白名单，未知 key 直接拒绝（`src-tauri/src/commands/settings.rs`）
- **3-7 scan方法统一DiskEntry**：将 `startup_scan` 内局部 `DiskEntry` 结构体移至 `helpers.rs` 作为公共结构，`full_scan`/`incremental_scan`/`startup_scan` 三处统一使用 `DiskEntry { abs_path, rel_path, size, name }`，消除 `Vec<String>` 与 `Vec<DiskEntry>` 不一致的问题（`src-tauri/src/scanner/helpers.rs`、`src-tauri/src/scanner/mod.rs`）
- **3-8 扩展名判断去重**：在 `extractor/mod.rs` 新增 `pub fn classify_ext(ext: &str) -> &str` 统一分类逻辑，`commands/files.rs` 中 5 处重复的 `matches!(ext.as_str(), ...)` 替换为 `classify_ext` 调用（`src-tauri/src/extractor/mod.rs`、`src-tauri/src/commands/files.rs`）
- **3-10 update_dir 触发 watcher 重启**：`update_dir` 更新目录配置后，停止并重启 watcher，使 exclude/include 模式变更即时生效（`src-tauri/src/commands/dirs.rs`）
- **3-13 add_dir 触发首次扫描**：`add_dir` 添加目录后，在启动 watcher 的同时异步触发 `incremental_scan`，确保新目录内容被立即索引（`src-tauri/src/commands/dirs.rs`）
- **3-14 搜索 page_size 上限保护**：`search` 命令的 `page_size` 增加 `.min(1000)` 上限，防止极端参数导致内存溢出（`src-tauri/src/commands/search.rs`）
- **3-15 download_files 临时目录清理**：`download_files` 已使用 `TempDir::new("ls_download")` 管理 zip 临时文件，Drop 时自动清理（`src-tauri/src/commands/files.rs`）
- **3-17 后台清理任务**：启动扫描完成后已在 `lib.rs` 调用 `cleanup_orphan_content` 和 `vacuum`，无遗漏（`src-tauri/src/lib.rs`）
- **3-18 移位检测 O(n·m)→O(n) 优化**：`startup_scan` 中移位检测由线性 `find` 改为按 `(name, size)` 建 `HashMap` 索引，将每次 DB 记录查找从 O(n) 降至 O(1)，整体从 O(n·m) 降至 O(n+m)（`src-tauri/src/scanner/mod.rs`）

---

## 2026-08-01 (今日)

### R4-C 前端质量改进
- **3-19 焦点判断改用 data-search-input**：SearchPage.tsx 原用 `placeholder.includes()` 判断焦点在搜索框，中文模式下失效。改为 `activeEl?.closest('[data-search-input]')`，与 SearchBar.tsx 的 `data-search-input="true"` 属性匹配（`src/pages/SearchPage.tsx`）
- **3-20 useIndexStatus 动态轮询间隔**：原固定 5s 轮询，改为 `is_scanning` 时 5s、空闲时 30s，减少索引空闲时的无效请求（`src/hooks/useIndexStatus.ts`）
- **3-22 SearchPage setTimeout 泄漏修复**：handleExport 内 3 个 `setTimeout` 无清理，用 `timersRef` + unmount effect 统一清理，防止组件卸载后状态更新崩溃（`src/pages/SearchPage.tsx`）
- **3-24 添加 ErrorBoundary**：新建 `src/components/ErrorBoundary.tsx`，在 App.tsx 外层包裹，渲染错误时显示"应用出错了，请重启或查看日志"而非白屏（`src/components/ErrorBoundary.tsx`、`src/App.tsx`）
- **3-25 暗色模式闪烁修复**：theme.tsx 原 `useState('light')` 初始值导致首次渲染闪烁。改为 `useMemo` 同步计算 resolved 值，DOM 初次渲染即正确（`src/theme.tsx`）
- **3-27 formatSize/formatTime 去重**：新建 `src/utils/format.ts` 统一工具函数，删除 PreviewPanel.tsx、ResultList.tsx 中的重复定义，减少维护成本（`src/utils/format.ts`、`src/components/PreviewPanel.tsx`、`src/components/ResultList.tsx`）
- **3-29 alert() 替换为 Tauri message()**：Settings.tsx 迁移失败提示原用浏览器 `alert()`，改为 `@tauri-apps/plugin-dialog` 的 `message()`，保持应用内 Dialog 风格一致（`src/pages/Settings.tsx`）

---

## 2026-08-01 (今日)

### 全量 i18n 改造
- **前端硬编码字符串全部提取到 en.ts / zh.ts**（`src/i18n/`）：新增 ~112 个翻译 key，覆盖 SearchPage、Browse、IndexStatus、DirManager、LogViewer、FileTypes、SearchBar、ResultList、PreviewPanel、FilterPanel、StatusBar、OnboardingWizard 共 12 个组件/页面
- **t() 支持参数**：`src/i18n/index.tsx` 扩展 `t(key, params?)` 签名，支持 `{placeholder}` 模板替换（如 `t('saved_to', { path })`、`t('results_count', { total })` 等）
- **SearchPage 键盘检查修复**：原检查 `placeholder.includes('your documents')` 在中文模式下失效，改为 `dataset.searchInput` 属性（`src/components/SearchBar.tsx` 加 `data-search-input="true"`，`src/pages/SearchPage.tsx` 改查该属性）
- **涉及文件**：`src/i18n/en.ts`、`src/i18n/zh.ts`、`src/i18n/index.tsx`、`src/pages/SearchPage.tsx`、`src/pages/Browse.tsx`、`src/pages/IndexStatus.tsx`、`src/pages/DirManager.tsx`、`src/pages/LogViewer.tsx`、`src/pages/FileTypes.tsx`、`src/components/SearchBar.tsx`、`src/components/ResultList.tsx`、`src/components/PreviewPanel.tsx`、`src/components/FilterPanel.tsx`、`src/components/StatusBar.tsx`、`src/components/OnboardingWizard.tsx`

---

## 2026-08-01

### 路径处理修复
- **to_relative 前缀误匹配**：原实现用 `path_str.starts_with(&root_str)` 字符串前缀比较，会把 `/tmp/foobar` 误认为 `/tmp/foo` 的子路径。改用 `Path::strip_prefix`（组件感知），并新增回归测试 `to_relative_respects_component_boundary`（`src-tauri/src/scanner/helpers.rs`）
- **路径迁移字节语义**：`migrate_paths_to_relative` 原用 SQL `SUBSTR(path, ?)` 按字节长度截断，中文等多字节路径会截错。改为 Rust 侧逐行迁移：按 `dir_id + prefix%` 查询后用 `path.strip_prefix(prefix)`（带 `/` 边界安全）更新（`src-tauri/src/db/tracker.rs`）

### 扫描统计与启动流程修复
- **扫描总耗时被覆盖**：`trigger_scan` 与 `rebuild_index` 中多目录扫描累加 `total_duration_ms = r.duration_ms` 每次都覆盖为最后一个目录的耗时，改为 `+=` 累加（`src-tauri/src/commands/index.rs`）
- **watcher 启动窗口期丢事件**：原启动流程先启动扫描线程、扫描完成后才发 `StartWatch`，扫描期间的文件变更因 watcher 未启动而丢失。改为先在主线程读取目录列表并发送 `StartWatch`，再启动扫描线程（`src-tauri/src/lib.rs`）
- **delete_file 静默吞错**：`mark_deleted` 失败被 `match` 静默忽略，改为 `if let Err(e)` 记录 `log::warn!`（`src-tauri/src/indexer.rs`）

### 搜索目录筛选修复
- **LIKE `%`/`_` 通配符转义**：dir_paths → file_ids 查询中 `p.replace('%', "%%")` 无效（SQLite LIKE 不识别 `%%`），改用 `ESCAPE '\'` 转义 `%` 和 `_`，避免含特殊字符的目录路径匹配错误。`search` 与 `export_search_results` 两处路径解析均已修复（`src-tauri/src/commands/search.rs`）

### TypeScript strict 模式
- **开启 TS strict 模式**：`tsconfig.app.json` 添加 `"strict": true`，符合 AGENTS.md 规范（strict + 禁止 any）。现有 34 个 TS 文件经 `tsc --noEmit -p tsconfig.app.json` 验证零错误
- **移除 SearchBar 中 `as any[]`**：`dropdown` 合并 suggestions（`string[]`）与 history（`SearchHistoryEntry[]`）改用展开语法 `[...suggestions, ...history]`，类型自然推断为 `(string | SearchHistoryEntry)[]`（`src/components/SearchBar.tsx`）

### 前端功能正确性修复
- **R3-2 预览高亮奇数次错乱**：`highlightText` 用带 `g` 标志的 `regex.test(part)` 判断是否高亮，`lastIndex` 状态导致奇数个匹配时高亮错乱。改用 `Set`（术语小写集合）做成员判断，正则仅用于切分（`src/components/PreviewPanel.tsx`）
- **R3-3 NumberField 清空输入回退异常**：`parseInt(e.target.value, 10) || min` 把空串/`NaN` 静默写成 `min` 且在输入过程中无法清空，改为 `Number.isNaN(v) ? min : Math.max(min, v)` NaN 安全钳制（`src/pages/Settings.tsx`）
- **R3-4 No Results 页 `<a href>` 整页跳转**：HashRouter 下 `<a href="/index">` 触发整页刷新，改用 `react-router-dom` 的 `<Link to="/index">`（`src/pages/SearchPage.tsx`）
- **R3-5 Enter 提交后 debounce 重复请求**：`submitSearch` 立即执行搜索后，300ms debounce effect 又因 query 变化触发一次同参数请求。新增 `lastSubmittedRef` 记录最近一次提交键 `query|page|sortField|sortOrder`，debounce effect 命中即跳过（`src/hooks/useSearch.ts`）
- **R3-14 Browse 搜索无防抖**：搜索框每个字符都触发一次 `listFilesDb` 请求。新增 `debouncedSearch` state + 300ms setTimeout 防抖，`loadFiles` 改用防抖后的值（`src/pages/Browse.tsx`）
- **R3-15 快速点击文件预览竞态**：慢返回覆盖快返回。新增 `previewVersionRef` 版本号，`selectFile` 每次自增并捕获本地版本，await 返回后版本不匹配则丢弃（`src/pages/Browse.tsx`）
- **R3-16 设置项每键写库**：`handleFieldChange` 每个字符都调 `updateSettings`，改用 `saveTimerRef` 300ms 防抖合并写入，卸载时清理未落盘的定时器（`src/pages/Settings.tsx`）

---

## 2026-07-30

### 项目初始化
- **ed1a639** Initial commit：Tauri 2 + React 19 + Tantivy 搜索引擎 + Tesseract OCR
- **874a0e4** chore：忽略 Tantivy 索引缓存文件

---

## 2026-07-31（第一轮：PaddleOCR + 启动流程 + Bug 修复）

### 🚀 PaddleOCR 内置引擎
- **`0e609c4`** feat: PaddleOCR 内置引擎 + 启动扫描 + 实时监控
  - 集成 `pure-onnx-ocr`（tract 纯 Rust ONNX 推理），PP-OCRv5 模型编译进二进制
  - 引擎优先级：PaddleOCR(默认) → Apple Vision → Windows OCR → Tesseract
  - `include_bytes!` 内嵌 21MB 模型，零外部依赖
  - 新增 `startup_scan()` 启动自动扫描
  - 实时文件监控（notify 300ms 防抖）
  - 文件移位检测（MD5 哈希匹配）
  - 默认排除规则（`#` `$` `.` `~` 前缀文件 + `.tmp` `.bak` 后缀等）
  - 移除全局快捷键 Ctrl+Space
  - 更新 README + USER_MANUAL

### 🔴 Bug 修复（12 项）`45db344`
1. `took_ms` 实为微秒 → `as_micros()` → `as_millis()`（searcher.rs）
2. `mem::forget(watcher)` 线程泄漏 → watcher 存入 AppState
3. MD5 哈希不一致（文件字节 vs 文本字节）→ 统一文件字节 MD5
4. `upsert_file` ON CONFLICT 错误重置 `indexed=0` → SQL 加 CASE WHEN
5. `last_scan` 秒 vs `mtime` 微秒精度不匹配 → `timestamp_micros()`
6. CSV 导出 path 列写成 file_name → SearchHit 加 path 字段
7. OCR 引擎检查与 PaddleOCR 默认冲突 → 匹配区分各引擎
8. FileWatcher 只处理 paths[0] → 遍历所有 paths
9. CSV 不转义特殊字符 → 所有列转义
10. `db_path.to_str().unwrap()` 非 ASCII 路径崩溃 → `to_string_lossy()`
11. OCR 预处理临时文件 PID 并发冲突 → UUID 替代 PID
12. macOS LibreOffice Dock 图标闪烁 → LSUIElement RAII guard（`bae64db`）

### 🏗️ 架构改进（16 项）
- **`c898d07`** 架构/性能/安全改进集
  - 定期 commit（每 100 文件自动提交）
  - IndexReader 复用（缓存 + reload）
  - `content_suggest` 字段用于搜索建议
  - `sort=name` Rust 侧排序
  - `filename:` 正则解析（支持任意位置）
  - CLI data_dir 统一
  - 移除非关键 unwrap/expect
  - PaddleOCR `Mutex + Send/Sync` 安全包装
  - 取消扫描功能（`cancel_scan` AtomicBool）
  - 清理孤儿 content_index
  - 数据库 VACUUM
- **`59bb801`** 流式MD5 + WalkDir 超时 + watcher 自动重连
  - MD5 流式计算（BufReader 替代 read_to_end）
  - 文件大小上限 100MB，超大文件只读首尾 1MB
  - WalkDir 计数 3 秒超时保护
  - FileWatcher 后台线程自动重连（3 次重试，500ms 间隔）
- **`75c7501`** Rayon 并行索引：`batch_index` par_iter 并行提取 + 串行 Tantivy 写入
- **`f82c645`** dead code 清理：`process_event`/`handle_create_modify`/`handle_delete`、`RawTokenizer`

### 🎨 前端假功能修复（8 项）`73489ef`
1. 排序选择器"死控件" → 打通前端→API→后端 sort/sortOrder
2. Pause/Resume 假按钮 → 改为取消扫描按钮
3. 文件类型分布假数据 → 新增 `get_file_type_stats` 命令
4. Recent Changes 计算错误 → 新增 `ScanDelta` 追踪真实数据
5. CSV 导出无保存对话框 → 系统 `save()` 对话框
6. DEBUG eprintln 遗留 → 删除

### 🟠 可用性改进（11 项）`789a648`
1. PDF 预览添加 📄 标识 + OCR 文字标题
2. 大文件预览截断 50k 字
3. 图片缩放控件 `[-][100%][+]`
4. Enter 键冲突修复（焦点在搜索框时不触发 openFile）
5. No results 引导：清空筛选 + 索引链接
6. 筛选持久化 localStorage
7. mtime 单位修复（`ts*1000` → `ts/1000`，后端微秒→前端 ms）全部 6 处
8. 侧边栏 File Types i18n
9. 搜索历史在输入时保留
10. 分页加页码输入跳转
11. 设置页自动保存，移除 Save 按钮

---

## 2026-07-31（第二轮：路径重构 + 迁移修复）

### 📁 相对路径存储
- **`843de19`** refactor: 文件路径由绝对→相对路径存储
  - `file_tracking` 和 Tantivy 索引 path 改为相对路径（相对 dir_config.path）
  - 新增 `to_relative()` / `to_absolute()` 辅助函数
  - 支持跨平台索引复用

### 🔧 修复
- **`8c66d08`** fix: LO 路径 onBlur 保存 + ScanDelta 真实 deleted/modified 值
- **`ead6023`** fix: batch 索引错误日志显示文件名+路径
- **`d599b64`** fix: 迁移数据后 data_dir 被设为消息字符串而非新路径
- **`0c65e66`** fix: 迁移数据完整修复（catch 缺失 + 允许空目录）
- **`e8d2ab2`** fix: get_stats 只统计活跃文件（`WHERE status='active'`）+ 绝对→相对路径自动迁移

---

## 2026-08-01（第三轮：扫尾 + 体验修复）

### 🔧 最后 5 项修复
- **`0c7f67f`** fix: `needs_reindex()` 抽取到 helpers.rs + ScanResult.added 分离 + list_dir_entries 过滤 deleted

### 📖 文档
- **`57dd72b`** docs: 基于项目现状全面重写 README 和用户手册

### 🚀 功能
- **`0ed36ae`** feat: 数据迁移后自动重启（`restart_app` 命令）
- **`19c595a`** feat: 设置页添加外部依赖面板（PaddleOCR/pdftoppm/LibreOffice 状态 + 一键复制安装命令）

### 🔧 修复
- **`6181000`** fix: 7个 TypeScript 编译错误
- **`eed560b`** fix: 迁移后改为确认对话框
- **`63d3d06`** fix: 索引状态页 Details 按钮无响应（`get_index_errors` 未注册 Tauri 命令）

---

## 2026-08-01（第四轮：更多 Bug + 文档 + 自动变更日志）

### 🔴 严重 Bug
- **`03949ac`** 修复 5 个 UX 缺陷
  - 删除文件无反应：`mark_deleted` SQL `WHERE path=?` 错误接收 UUID，改为 `WHERE id=?`
  - `.DS_Store` 被实时索引：`handle_event` watcher 回调遗漏 `is_excluded` 检查
  - 设置页安装命令显示三个平台：前端按 `navigator.platform` 过滤当前平台
  - LO 路径输入与依赖检测分离：合并到依赖面板同一行
  - 索引状态 `pending` 和 `errors` 关系不清：Pending 卡片加 `incl. errors` 副标题
- **`ae3857c`** 索引期间 UI 冻结：r2d2 连接池仅 8 个，Rayon 并行任务耗尽连接，前端 IPC 命令 `get()` 阻塞 → `max_size: 8→32` + `connection_timeout: 10s`
- **`8f8980c`** 启动扫描 VACUUM 阻塞：VACUUM 持有 SQLite 独占锁，移到 watcher 之后执行 + 发 `scan-completed` 事件

### 🟠 功能修复
- **`63d3d06`** Details 按钮无响应：`get_index_errors` 命令未注册为 Tauri handler，前端 `invoke` 静默失败
- **`0c65e66`** 迁移数据路径错误：`migrateData` 返回消息字符串，前端误当路径存 → 改 `selected` + 加 catch 弹窗
- **`0ed36ae`** 迁移后自动重启：新增 `restart_app` Tauri 命令 + 确认对话框
- **`19c595a`** 设置页外部依赖面板：PaddleOCR/pdftoppm/LibreOffice 状态 + 一键复制安装命令
- **`6181000`** 7 个 TS 编译错误：泛型类型错误 + 未使用导入 + API 签名变更

### 📖 文档
- **`57dd72b`** README + 用户手册全面重写
- **CHANGELOG.md** 首次创建（27 个 commit 完整记录）

### 🔧 工作流
- **`0adfab5`** 自动变更日志：Git post-commit hook 首次尝试 → 改为 AI 手动编写详细条目
- **`12a678b`** 添加 `AGENTS.md` 项目规范：变更记录规则、代码规范、关键文件索引

---

## 2026-08-01（第五轮：Browse 页重写为表格视图）

### 🚀 新功能
- **`a2e0e16`** Browse 页全面重写：从文件系统目录树浏览改为数据库驱动的表格视图
  - 新增后端 `list_files_db` 命令：分页查询 `file_tracking` 表，支持状态筛选（全部/已索引/未索引/失败）、文件类型筛选、文件名模糊搜索、多字段排序（名称/路径/类型/大小/时间）
  - 前端表格列：文件名（ellipsis 截断）| 路径（ellipsis + title 完整路径）| 类型 | 状态（✓/✗/○ 图标）
  - 工具栏：状态筛选下拉 + 类型筛选 + 搜索框 + 排序选择
  - URL `useSearchParams` 同步所有筛选状态，刷新/分享不丢失
  - 分页控件（上/下页 + 页码跳转）
  - 点击行 → 右侧预览面板（复用 PreviewPanel）
  - 移除旧的目录树递归逻辑和相关 state

### 🟠 IndexStatus 卡片跳转
- 索引状态页 StatCard 支持跳转：Total Files → Browse，Indexed → `?filter=indexed`，Pending → `?filter=pending`。OCR'd 跳全部（暂无对应筛选），Errors 保留展开详情功能

---

## 2026-08-01（第六轮：扫描流程 + 数据一致性修复）

### 🔴 严重 Bug
- **`b1ba768`** list_files_db SQL 参数错位：`where_clause` 的 `?` 占位符与 `LIMIT ? OFFSET ?` 位置冲突导致查询失败，Browse 页无内容 → 改用 `params_from_iter` 正确绑定；`sort=name` 改用 `path` 排序（file_name 不是 DB 列）
- **`b1ba768`** 删除目录后残留数据：`remove_dir` 只删 `dir_config` 行，file_tracking 孤儿记录（统计虚高）、Tantivy 文档（仍可搜索）、content_index 引用全部残留 → 增加清理：先按 dir_id 从 Tantivy 删文档，再硬删 `file_tracking` 行，最后 `cleanup_orphan_content` 清理孤儿 content

### 🚀 新功能
- **`ede3cce`** 扫描两阶段进度报告：`ScanProgress` 增加 `phase` 字段（`"scan"`/`"index"`），`batch_index` 增加进度回调，Phase 2 串行写入时每处理一个文件上报已索引数；三个扫描函数 walk 阶段发 `phase:"scan"`、索引阶段发 `phase:"index"`，前端状态栏和索引状态页据此显示"正在扫描/正在索引"

### 📖 文档
- **`1b06f2c`** 添加完整 CHANGELOG.md
- **`03684db`** 修复 CHANGELOG 格式

### 🏗️ 索引目录命名重构
- **索引目录撞车**：data_dir 名为 "index" 时与硬编码索引子目录 `data_dir/index` 撞车，产生双重 `index/index`。新增共享常量 `INDEX_DIR_NAME = ".ls-index"`，替换全部硬编码 `join("index")`（`lib.rs`/`cli.rs`/`commands/config.rs`/`commands/backup.rs`）。启动时检测旧布局 `data_dir/index` 并重命名为 `.ls-index`（幂等）。`phase: "index"` 扫描标记与 `data.db` 路径逻辑不受影响

---

## 2026-08-01

### 🏗️ 索引重建改为原子替换
- **重建中断不丢旧索引**：`rebuild_index`（`commands/index.rs`）不再先 `remove_dir_all` 删旧索引，改为：① 建临时目录 `index.tmp-<uuid>`（`uuid::Uuid::new_v4().simple()`，同父目录下 `with_file_name`）→ ② 清空 `file_tracking`/`content_index`（保留原逻辑）→ ③ 在 tmp 目录 `IndexManager::open_or_create` 并 swap 内存 → ④ `reset_writer` → ⑤ 全量扫描（逻辑不变，写入 tmp 索引）→ ⑥ `indexer.commit()` 确保落盘 → ⑦ 原子替换：旧目录 rename 为 `index.old`，tmp rename 为 `index_dir`，成功则删 backup，失败则回滚还原旧索引。所有错误退出路径清理 tmp_dir 并复位 `is_scanning`/`is_rebuilding`/`cancel_scan`。搜索已被 `is_rebuilding` 守卫，重建期间读旧索引不受影响

### 🚀 重建期间搜索守卫
- **重建索引时搜索返回友好错误**：`AppState` 新增 `is_rebuilding: Arc<AtomicBool>` 标志（`state.rs`），`rebuild_index` 启动时置 true、所有退出路径（含 spawn_blocking 内提前 return 与正常结束）置 false（`commands/index.rs`）；`search` 命令开头检查该标志，重建期间直接返回 `"索引重建中，请稍后再试"`（`commands/search.rs`）。`lib.rs` / `tests/ipc_test.rs` 的 `AppState::new` 调用点传入新参数。未改动 rebuild 的目录删除/重建逻辑（R1-3b 单独处理）

### 🔒 安全加固（2 项）
- **Tauri CSP 启用**：`tauri.conf.json` 中 `"csp": null` → 完整 CSP 策略（`default-src 'self'` + 白名单 script/style/img/media/connect/frame/font/worker）。`connect-src` 额外加入 `http://ipc.localhost` 以兼容 Windows/Linux 的 IPC 通道（macOS 走 `ipc://`），防止 IPC 被 CSP 阻断。前端仅本地资源，无远程内容受影响
- **fs 插件权限收窄 + scope**：`capabilities/default.json` 删除 `fs:allow-mkdir` / `fs:allow-remove` / `fs:allow-rename`（前端未使用）；保留读权限与 `fs:allow-write`（SearchPage.tsx 的 CSV 导出 `writeTextFile` 依赖它，且 `save()` 对话框会自动将选中路径加入 fs scope，导出不受影响）；新增 `fs.scope` 白名单（`$APPDATA` / `$APPLOCALDATA` / `$DOCUMENT` / `$DESKTOP` / `$DOWNLOAD` 递归）

### 🔴 Bug 修复
- **lock_writer 并发丢索引**：`lock_writer`（`indexer.rs`）先释放 writer 锁再创建 `IndexWriter`，两个线程并发首次写入时各建一个 writer，后写者覆盖前者、丢文档。改为全程持锁创建（`index_manager` 是 RwLock 读锁、不依赖 writer 锁，不会死锁）
- **切换语言清空 data_dir**：`setLang`（`i18n/index.tsx`）调用 `updateConfig({ data_dir: '', language: l })` 把配置里的 data_dir 清空，切换语言即丢全部数据。改为只传 `{ language: l }`；同时在 `updateConfig`（`api/config.ts`）加防呆，拒绝空 `data_dir` 并抛错 `data_dir cannot be empty`，从源头杜绝此类覆盖
- **上次取消后下次扫描立即被取消**：`cancel_scan` 标志在扫描开始时未复位，上次点过取消后，`trigger_scan`/`rebuild_index` 的循环第一次 `load` 就为 true 直接 break → 两个 `spawn_blocking` 闭包开头先 `cancel_scan.store(false, Ordering::Release)` 复位；循环内 `load(Relaxed)` 改为 `Acquire`，`cancel_scan` 命令 `store(true, ...)` 改为 `Release`，形成 acquire-release 同步对（`commands/index.rs`）
- **restore_backup 直接覆盖活跃 data.db 损坏数据库**：WAL 模式下连接池仍持有 data.db，`fs::copy` 覆盖与 WAL 冲突可能导致损坏。改为 SQLite 在线备份 API（`rusqlite::backup::Backup::new(&src, &mut dst)` + `step(-1)`，`step_to` 在 0.32 已改名；Busy/Locked 重试 3 次）从备份的 data.db 恢复到活跃连接，不再直接覆盖文件。索引目录改为 rebuild_index 的 tmp→rename 原子替换 + 切换 IndexManager。恢复完成 emit `restore-completed` 后自动重启生效。`AppState` 新增 `is_restoring: Arc<AtomicBool>` 防重入（`state.rs`/`lib.rs`/`tests/ipc_test.rs`，顺带修复 ipc_test 缺参编译错误）；`Cargo.toml` 启用 rusqlite `backup` feature（`commands/backup.rs`）

---

## 2026-08-01

### 🏗️ 临时目录 RAII 工具 TempDir
- **临时目录并发冲突与泄漏**：4 处用 `ls_*_{pid}` 命名系统临时目录的代码，并发/多实例运行时共享同一路径互相覆盖，且提前 return 时遗留垃圾目录。新增 `scanner/helpers.rs` 的 `TempDir`（`{prefix}_{pid}_{uuid}` 唯一路径 + Drop 自动 `remove_dir_all`），替换 4 处：
  - `commands/files.rs` `download_files`：`ls_download_{pid}` → `TempDir::new("ls_download")`（zip 打包）
  - `commands/search.rs` `export_search_results`：`ls_export_{pid}.{format}` → `TempDir::new("ls_export")`（CSV/文本导出）
  - `extractor/office/mod.rs` `extract_via_libreoffice`：`ls_lo_{pid}` → `TempDir::new("ls_lo")`（guard 留在函数作用域，路径 clone 进线程，移除手动 `remove_dir_all`）
  - `extractor/pdf.rs` `ocr_pdf_via_pdftoppm`：`ls_pdf_ocr_{pid}` → `TempDir::new("ls_pdf_ocr")`（移除手动 `remove_dir_all`）
  - 新增 2 个单测：drop 后目录被删除、路径唯一（`cargo test --lib scanner::helpers` 通过）

## 2026-08-01

### 🏗️ Tantivy path 字段改为相对路径
- **索引内 path 存绝对路径、与 DB 不一致**：`batch_index` 和 `index_file` 用 `file_path.to_string_lossy()`（绝对路径）写入 Tantivy 的 `path` 字段，而 `file_tracking.path` 存相对路径，搜索结果路径与 Browse 页不一致。修复：
  - `batch_index`：`ExtractedData.file_path_str` 改用 `job.rel_path`（`BatchJob.rel_path` 已由 scanner 传入，DB 也用它做 upsert）
  - `index_file`：调用方未传 rel_path 时，从 `dir_config::get_dir` 取目录根 + `helpers::to_relative` 补算相对路径；读文件内容仍用绝对路径（`file_path` 参数），仅写索引用相对路径（`indexer.rs`）
  - 存量绝对路径：无需单独迁移——`startup_scan` 每次启动会用 rel_path 重写索引；若搜索结果路径仍显示绝对路径，在索引状态页重建索引一次


## 统计

| 类别 | 数量 |
|------|:---:|
| 🔴 Bug 修复 | 30+ |
| 🏗️ 架构/性能改进 | 20 |
| 🎨 UI/UX 修复 | 25+ |
| 🚀 新功能 | 8 |
| 📖 文档 | 5 |
| **总计 commits** | **35** |
| **变更文件数** | **70+** |

## 2026-08-14（E2E 自动化测试 + 用户手册更新 + 截图）

- **集成 tauri-plugin-mcp**：git 依赖，`#[cfg(debug_assertions)]` 条件编译，生产构建自动排除；`src/App.tsx` 挂载时调用 `setupPluginListeners()`（`src-tauri/Cargo.toml`、`src-tauri/src/lib.rs`、`src/App.tsx`）
- **创建 .opencode/agents 自定义 Agent**：`tauri-auto-ui-gen.yaml`，用于 MCP 连接 App 自动生成 E2E 测试
- **Mock IPC 测试 `auto_ui_e2e.rs`**：15 个用例，覆盖搜索/浏览/目录/索引/设置/日志/文件类型/AI 聊天会话 CRUD/版本，全部通过（`src-tauri/tests/auto_ui_e2e.rs`）
- **MCP E2E 测试 `e2e_mcp.sh`**：22 个用例，覆盖 8 个页面路由加载 + AI 流式对话全链路（打字/发送/等待响应/验证回答/证据面板），全部通过（`src-tauri/tests/e2e_mcp.sh`）
- **更新用户手册**：各章节验证清单改为 E2E 覆盖表格；搜索页补充空态文字和筛选面板文件列表；索引状态页补充操作按钮和统计卡片详情；AI 聊天页补充会话列表/文件树/输入框交互细节（`docs/01-install.md`、`docs/04-search.md`、`docs/05-browse.md`、`docs/06-settings.md`、`docs/07-index-manage.md`、`docs/08-ai-features.md`、`docs/USER_MANUAL.md`、`USER_MANUAL.md`）
- **新增测试文档**：`docs/12-testing.md`，详述 37 个 E2E 用例覆盖范围、运行方式、常见问题
- **更新截图**：全部 8 页 + 5 个设置标签 + 5 张标注截图，截取自运行中 App 实时画面（`docs/screenshots/`）
- **修复模型路由**：`9Router` vs `9router` 大小写导致子代理 ProviderModelNotFoundError，统一 provider 名（`~/.config/opencode/opencode.json`）

---

## 路线图

### 系统托盘（优先级：低）

**理由**：托盘功能在 macOS 上遇到 Tauri 2 的平台限制——设置菜单后左键点击事件被系统拦截（`Click` 不发送），不设菜单则左键可工作但无菜单项。当前 macOS 上托盘没有实用价值，暂移除代码。

**待实现方案**（任选一个）：
- **方案 A**：macOS 上用 `TrayIconEvent::Click` 切换窗口，用 `TrayIconEvent::RightClick` 弹出自定义菜单（需调研 Tauri 2 是否支持 `RightClick` 事件）
- **方案 B**：移除菜单 + 用全局快捷键（如 `Cmd+Shift+L`）替代托盘功能，托盘仅做窗口最小化指示
- **方案 C**：等待 Tauri 2 更新对 macOS 托盘菜单事件的支持，或改用 `tao` 底层 API 直接操作 `NSStatusItem`
