# Link-Searcher 架构文档

> 面向开发者的完整技术文档。涵盖 AI RAG 管线、全文搜索、前端架构、数据库、事件系统、Web API、调试诊断。

---

## 目录

1. [技术栈](#技术栈)
2. [项目结构总览](#项目结构总览)
3. [数据库 Schema](#数据库-schema)
4. [Tauri IPC 命令参考](#tauri-ipc-命令参考)
5. [AI RAG 管线详解](#ai-rag-管线详解)
6. [内容注入与截断策略](#内容注入与截断策略)
7. [事件系统](#事件系统)
8. [Web API 服务器](#web-api-服务器)
9. [前端架构](#前端架构)
10. [配置系统](#配置系统)
11. [构建与部署](#构建与部署)
12. [测试](#测试)
13. [调试与诊断](#调试与诊断)
14. [已知缺陷与设计约束](#已知缺陷与设计约束)

---

## 技术栈

| 层 | 技术 | 版本 |
|-----|------|------|
| 桌面框架 | Tauri 2.x | 2.x |
| 前端 | React 19 + TypeScript + Tailwind CSS 4 | 19 / 4.x |
| 全文搜索 | Tantivy + jieba-rs | 0.22 |
| 向量检索 | BGE-large-zh-v1.5（tract ONNX） | v1.5 |
| LLM 网关 | OpenAI 兼容 API | — |
| 数据库 | SQLite（rusqlite + r2d2） | 3.x |
| 异步运行时 | tokio（多线程） | 1.x |
| 并行处理 | Rayon | 1.x |
| HTTP 服务 | axum + axum-server（TLS） | 0.8 / 0.10 |
| 构建工具 | Vite + Cargo | 6.x / 1.85+ |

---

## 项目结构总览

```
link-searcher/
├── src/                          # React 前端（TypeScript）
│   ├── main.tsx                  # 入口
│   ├── App.tsx                   # 根组件（路由、主题、Token 管理）
│   ├── api/
│   │   ├── client.ts             # 统一 API 客户端（Tauri IPC / HTTP fetch）
│   │   ├── files.ts              # 文件/会话/流式 API 封装
│   │   └── search.ts             # 搜索 API
│   ├── components/
│   │   ├── ChatPanel.tsx         # AI 聊天面板（755 行，核心组件）
│   │   ├── AiEventTimeline.tsx   # AI 推理过程时间线
│   │   ├── AiEvidencePanel.tsx   # 检索依据面板
│   │   ├── ResultList.tsx        # 搜索结果列表
│   │   ├── PreviewPanel.tsx      # 文件预览
│   │   └── ...
│   ├── pages/
│   │   ├── AiChat.tsx            # AI 聊天页（会话管理）
│   │   ├── SearchPage.tsx        # 搜索页
│   │   ├── Browse.tsx            # 浏览页
│   │   └── ...
│   ├── hooks/                    # 自定义 Hooks
│   ├── i18n/                     # 国际化（zh/en/ja/ko）
│   └── utils/
│       └── platform.ts           # 平台检测（Tauri vs 浏览器）
│
├── src-tauri/                    # Rust 后端
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs               # 入口：GUI 或 CLI 分发
│   │   ├── lib.rs                # Tauri 启动 + Web API 启动
│   │   ├── cli.rs                # 命令行接口
│   │   ├── config.rs             # 配置管理（AI 端点、目录、语义权重）
│   │   ├── state.rs              # AppState 全局状态
│   │   ├── boot.rs               # 核心组件初始化
│   │   ├── indexer.rs            # 批量索引（Rayon 并行 + Tantivy 串行写入）
│   │   ├── search/               # Tantivy 搜索引擎
│   │   │   ├── mod.rs            # IndexManager
│   │   │   ├── schema.rs         # 字段定义 + tokenizer 注册
│   │   │   ├── indexer.rs        # 文档增删改
│   │   │   └── searcher.rs       # 搜索/建议/导出
│   │   ├── db/                   # SQLite 数据库
│   │   │   ├── tracker.rs        # 文件追踪 CRUD（file_tracking 表）
│   │   │   ├── chunks.rs         # 分块存储（doc_chunks 表）
│   │   │   ├── ai_events.rs      # AI 事件记录（ai_events 表）
│   │   │   └── ...
│   │   ├── extractor/            # 文本提取管线
│   │   │   ├── mod.rs            # 格式路由
│   │   │   ├── pdf.rs            # PDF 提取（lopdf + pdftoppm OCR）
│   │   │   ├── office/           # Office 提取（rwml/calamine/anydoc）
│   │   │   ├── image.rs          # 图片 OCR
│   │   │   ├── audio.rs          # 音频转写（FunASR-Nano + 说话人分离）
│   │   │   ├── ocr.rs            # OCR 引擎调度
│   │   │   ├── paddleocr.rs      # PaddleOCR 内置引擎
│   │   │   └── text.rs           # 纯文本提取
│   │   ├── scanner/              # 目录扫描 + 文件监控
│   │   │   ├── mod.rs            # 全量/增量/启动扫描
│   │   │   ├── watcher.rs        # 实时文件监控（notify + 300ms 防抖）
│   │   │   └── helpers.rs        # 排除规则 + 路径转换
│   │   ├── ai/                   # AI 核心模块
│   │   │   ├── mod.rs            # LLM 调用（chat/chat_stream/embed/vector_full_scan）
│   │   │   ├── local_embed.rs    # 本地 BGE 嵌入引擎（tract ONNX + tokenizers）
│   │   │   └── skills/           # RAG 管线 Skills
│   │   │       ├── pipeline.rs         # RAGPipeline 编排器
│   │   │       ├── query_rewrite.rs    # 查询改写
│   │   │       ├── scope_resolver.rs   # 检索范围解析
│   │   │       ├── retrieval.rs        # BM25 + 语义检索
│   │   │       └── context_assembly.rs # 上下文组装
│   │   ├── commands/             # Tauri IPC 命令
│   │   │   ├── ai.rs             # AI 对话/问答/摘要（3066 行）
│   │   │   ├── search.rs         # 搜索
│   │   │   ├── files.rs          # 文件操作
│   │   │   ├── index.rs          # 索引管理
│   │   │   ├── config.rs         # 配置读写
│   │   │   ├── settings.rs       # 设置管理
│   │   │   ├── backup.rs         # 备份恢复
│   │   │   ├── dirs.rs           # 目录管理
│   │   │   ├── bge.rs            # BGE 模型下载
│   │   │   ├── funasr.rs         # FunASR 模型下载
│   │   │   └── tesseract.rs      # Tesseract OCR 管理
│   │   ├── webapi/               # 可选 HTTPS Web API
│   │   │   ├── mod.rs            # 服务器启动 + 事件桥（BRIDGED_EVENTS）
│   │   │   ├── auth.rs           # Bearer token 认证中间件
│   │   │   ├── state.rs          # ApiState
│   │   │   ├── tls.rs            # 自签名 TLS 证书管理
│   │   │   ├── static_files.rs   # 前端静态文件（dist/）
│   │   │   └── routes/
│   │   │       ├── mod.rs        # 路由注册
│   │   │       ├── ai.rs         # AI 端点（对话/摘要/会话）
│   │   │       ├── search.rs     # 搜索端点
│   │   │       ├── files.rs      # 文件端点
│   │   │       ├── events.rs     # SSE 事件桥（/api/events）
│   │   │       └── ...
│   │   └── logs/
│   │       └── session.rs        # 日志会话管理
│   ├── models/                   # PaddleOCR ONNX 模型
│   ├── capabilities/             # Tauri 权限配置
│   └── tests/                    # 集成测试
│
├── docs/                         # 文档
│   ├── USER_MANUAL.md            # 用户手册入口
│   ├── ARCHITECTURE.md           # 本文档
│   ├── SEARCH_UX_IMPLEMENTATION.md
│   └── 01-*.md ~ 12-*.md        # 手册章节
│
├── USER_MANUAL.md                # 用户手册入口
├── README.md                     # 项目 README
├── CHANGELOG.md                  # 变更日志
├── AGENTS.md                     # Agent 开发规范
├── ROADMAP.md                    # 路线图
├── package.json                  # npm 依赖
├── vite.config.ts                # Vite 配置
└── tsconfig.json                 # TypeScript 配置
```

---

## 数据库 Schema

### `file_tracking` — 文件追踪

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | TEXT PK | UUID |
| `path` | TEXT UNIQUE | 相对路径（相对于监控目录根） |
| `file_ext` | TEXT | 扩展名 |
| `dir_id` | TEXT FK | 所属目录配置 |
| `mtime` | INTEGER | 修改时间戳 |
| `size` | INTEGER | 文件大小（字节） |
| `md5` | TEXT | MD5 内容哈希（流式前 1MB） |
| `status` | TEXT | `active` / `deleted` / `missing` |
| `indexed` | INTEGER | 0=未索引, 1=已索引, 2=提取中, 3=已提取 |
| `error_msg` | TEXT | 错误信息 |
| `dead_content` | INTEGER | 0=正常, 1=内容已失效 |
| `created_at` | INTEGER | 创建时间戳 |
| `updated_at` | INTEGER | 更新时间戳 |

索引：`idx_ft_dir_id`, `idx_ft_status`, `idx_ft_md5`, `idx_ft_mtime`, `idx_ft_pending(WHERE indexed IN (0,2,3))`

### `content_index` — 文本内容

| 字段 | 类型 | 说明 |
|------|------|------|
| `md5` | TEXT PK | 文件 MD5 |
| `text_content` | TEXT | 提取的文本内容 |
| `char_count` | INTEGER | 字符数 |
| `ocr_used` | INTEGER | 0=文本提取, 1=OCR 提取 |
| `ocr_duration_ms` | INTEGER | OCR 耗时（毫秒） |
| `indexed_at` | INTEGER | 索引时间 |

### `doc_chunks` — 文档分块

| 字段 | 类型 | 说明 |
|------|------|------|
| `md5` | TEXT | 文件 MD5 |
| `chunk_index` | INTEGER | 分块序号 |
| `start_char` | INTEGER | 起始字符位置 |
| `end_char` | INTEGER | 结束字符位置 |
| `text` | TEXT | 分块文本 |

主键：`(md5, chunk_index)`

### `doc_embeddings` — 语义向量

| 字段 | 类型 | 说明 |
|------|------|------|
| `file_id` | TEXT PK | 文件 ID |
| `dim` | INTEGER | 向量维度 |
| `vector` | BLOB | 序列化的 f32 向量 |
| `updated_at` | INTEGER | 更新时间 |

### `doc_summaries` — 文档摘要

| 字段 | 类型 | 说明 |
|------|------|------|
| `file_id` | TEXT PK | 文件 ID |
| `summary` | TEXT | LLM 生成的摘要 |
| `updated_at` | INTEGER | 更新时间 |

### `ai_events` — AI 推理事件

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | INTEGER PK | 自增 |
| `session_id` | TEXT | 会话 ID |
| `turn_number` | INTEGER | 轮次（0-based） |
| `event_seq` | INTEGER | 事件序号 |
| `event_type` | TEXT | `query_rewrite` / `scope_resolved` / `retrieval` / `context_assembled` / `llm_call` / `turn_complete` |
| `payload_json` | TEXT | JSON 事件数据 |
| `created_at` | INTEGER | 时间戳 |

索引：`idx_ae_session(session_id, turn_number)`, `idx_ae_created_at`

### `dir_config` — 目录配置

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | TEXT PK | UUID |
| `path` | TEXT UNIQUE | 绝对路径 |
| `alias` | TEXT | 别名 |
| `ocr_lang` | TEXT | OCR 语言（默认 `eng`） |
| `exclude_patterns` | TEXT | 排除 glob 规则 |
| `include_exts` | TEXT | 白名单扩展名 |
| `recursive` | INTEGER | 是否递归 |
| `created_at` | INTEGER | 创建时间 |
| `updated_at` | INTEGER | 更新时间 |

### `app_settings` — 应用设置

| 字段 | 类型 | 说明 |
|------|------|------|
| `key` | TEXT PK | 键 |
| `value` | TEXT | 值 |

常用键：`web_api_enabled`, `web_api_port`, `web_api_token`, `web_api_bind`, `web_api_dev_mode`

### 其他表

- `search_history`：搜索历史（支持置顶）
- `index_errors`：索引错误记录（按类型分类）
- `hotword_counts`：热词统计
- `unsupported_ext_stats`：不支持的扩展名统计

---

## Tauri IPC 命令参考

所有命令注册在 `src-tauri/src/lib.rs` 的 `invoke_handler` 中。

### AI 对话

| 命令 | 签名 | 说明 |
|------|------|------|
| `conversation_ask_stream` | `(messages, source_ids, session_id, scope, sessionRetrievalScope, strict_docs, full_recall) → void` | 流式 AI 对话，emit `ai-chunk`/`ai-done` |
| `conversation_ask` | `(同上) → String` | 非流式 AI 对话 |
| `smart_search_stream` | `(query, session_id) → void` | 流式智能搜索 |
| `smart_search` | `(query) → SmartSearchResponse` | 非流式智能搜索 |
| `ask_documents` | `(file_ids, question) → String` | 基于选中文件回答 |
| `cancel_ai_request` | `() → void` | 取消当前 AI 请求 |

### AI 能力

| 命令 | 签名 | 说明 |
|------|------|------|
| `ai_capabilities` | `() → AiCapabilities` | 返回 `{ embedding: bool, llm: bool }` |
| `test_ai_gateway` | `() → Vec<GatewayTest>` | 测试网关连通性 |
| `summarize_file` | `(file_id) → SummaryResult` | LLM 生成文档摘要 |
| `ai_topic_clusters` | `(file_ids, question) → Vec<TopicCluster>` | 文档主题聚类 |
| `batch_summarize` | `(file_ids, query) → (String, Vec<String>)` | 批量文档摘要 |

### 会话管理

| 命令 | 签名 | 说明 |
|------|------|------|
| `list_chat_sessions` | `() → Vec<ChatSessionMeta>` | 列出所有会话 |
| `create_chat_session` | `() → String` | 创建新会话，返回 ID |
| `load_chat_session` | `(id) → Option<ChatSession>` | 加载会话 |
| `save_chat_session` | `(session) → void` | 保存会话 |
| `delete_chat_session` | `(id) → void` | 删除会话 |
| `export_chat_session` | `(id) → String` | 导出为 Markdown |
| `get_ai_events` | `(session_id) → Vec` | 获取会话所有 AI 事件 |
| `get_turn_ai_events` | `(session_id, turn_number) → Vec` | 获取单轮 AI 事件 |

### 搜索

| 命令 | 签名 | 说明 |
|------|------|------|
| `search` | `(params) → SearchResponse` | 全文搜索 |
| `suggest` | `(query) → Vec<String>` | 搜索建议 |
| `export_search_results` | `(query, dir_ids, ext_filter, format) → void` | 导出搜索结果 |
| `get_search_history` | `() → Vec` | 搜索历史 |
| `clear_search_history` | `() → void` | 清除搜索历史 |

### 数据流事件

```rust
// 后端 emit 事件
AiProgress { session_id, phase, message, current, total }  // "ai-progress"
AiChunk    { session_id, delta, reasoning }                 // "ai-chunk"
AiDone     { session_id, full_text, took_ms, cancelled, source_ids, source_files, evidence, ... } // "ai-done"
```

### 前端会话状态

```typescript
interface ChatSession {
  id: string
  title: string
  messages: ChatMessage[]
  source_ids: string[]
  source_files: string[]
  strict_docs: boolean
  full_recall: boolean
  retrieval_scope: string[]
  pending_query: string | null
  pending_started_at: number | null  // loading 状态驱动
  per_turn_evidence: TurnEvidence[]
  per_turn_scopes: TurnScope[]
}

---

## AI RAG 管线详解

### 整体流程

```
用户输入 "判决结果是什么？"
        │
        ▼
┌──────────────────────────────────────────────────────────────┐
│  Step 1: QueryRewrite（查询改写）                              │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │ 规则改写（rewrite_query）                                  │ │
│  │  - 指代消解：上一个问句的关键词 + 当前问句合并               │ │
│  │  - 停止词过滤：去掉"它/这/那/的/了/吗"等                    │ │
│  │  - 短问句继承父关键词："增量呢" → "营收 增量"               │ │
│  ├─────────────────────────────────────────────────────────┤ │
│  │ LLM 改写（llm_rewrite_query，可选）                         │ │
│  │  - 调用 chat() 补全指代与省略                               │ │
│  │  - 5 秒超时保护，失败回退到规则改写                          │ │
│  │  - 验证：非空、不超过 80 字符、非原样                        │ │
│  └─────────────────────────────────────────────────────────┘ │
│  → search_q: "判决结果"                                        │
└──────────────────────┬───────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────────────┐
│  Step 2: ScopeResolver（范围解析）                             │
│  输入：                                                        │
│   - session_retrieval_scope: 跨轮累计范围                      │
│   - TurnScope.mention_files: @mention 文件列表                │
│   - TurnScope.mention_dirs: @mention 目录列表                  │
│   - TurnScope.conditions: /ext: /date: /范围: 命令            │
│  输出：                                                        │
│   - dir_ids: 监控根目录 ID（数据库精确匹配）                     │
│   - path_prefixes: 子目录路径前缀（LIKE 回退）                  │
│   - mention_file_ids: 引用文件 ID（精确匹配）                   │
│   - ext_filter / date_from / date_to                         │
│   - mention_resolved / missing_mentions                       │
└──────────────────────┬───────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────────────┐
│  Step 3: Retrieval（三路检索合并）                             │
│  ① BM25 全文检索（Tantivy）                                     │
│     - 索引字段：content（正文）+ path（文件名）                   │
│     - 支持 dir_ids / file_ids / ext / date / path_prefixes   │
│  ② 语义向量全量扫描（vector_full_scan）                          │
│     - embed(query) → BGE 向量                                   │
│     - 遍历 doc_embeddings 全表，计算 cosine 相似度               │
│     - 过滤 ≥ VECTOR_THRESHOLD(0.65) 的结果                      │
│     - full_recall=false 时最多 500 条                            │
│  ③ SQL 路径匹配（path_match_files）                              │
│     - LIKE 模糊匹配文件名关键词                                    │
│  三路合并去重：HashSet<file_id> → all_hits                       │
└──────────────────────┬───────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────────────┐
│  Step 4: ContextAssembly（三层上下文注入）                      │
│  Layer 0: 引用文档全部注入（mention_budget = BUDGET/3）         │
│  Layer 1: BM25 命中注入（full_recall决定 30 或全量）             │
│  Layer 2: 剩余命中摘要（batch_summarize，最多 60 份）            │
│  最终截断：truncate_text(docs, max_context_chars)              │
│  → system prompt + user_msg                                   │
└──────────────────────┬───────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────────────┐
│  Step 5: LLM 调用（chat_stream）                              │
│  POST {base_url}/chat/completions                             │
│  { model, messages, temperature: 0.3, max_tokens: 4096,      │
│    stream: true }                                             │
│  SSE 流式解析 → emit("ai-chunk") → emit("ai-done")            │
│  降级：gateway 忽略 stream:true 时回退到非流式                   │
│  取消：每行读取后检查 ai_cancelled()                             │
└──────────────────────────────────────────────────────────────┘
```

### 关键常量

| 常量 | 值 | 位置 | 说明 |
|------|-----|------|------|
| `CONTEXT_BUDGET` | 150,000 chars | `commands/ai.rs:862` | 总上下文预算 |
| `SYSTEM_OVERHEAD` | 2,000 chars | `commands/ai.rs:863` | system prompt 模板开销 |
| `ANSWER_RESERVE` | 8,000 chars | `commands/ai.rs:864` | 为 LLM 回答预留空间 |
| `MAX_CONTENT_INJECT` | 30 | `commands/ai.rs:1154` | Layer 1 注入上限（关闭全量召回时） |
| `MAX_REMAINING_SUMMARY` | 60 | `commands/ai.rs:1255` | Layer 2 摘要上限 |
| `MAX_CONCURRENCY` | 6 | `commands/ai.rs:1639` | batch_summarize 并行 LLM 调用数 |
| `BATCH_SIZE` | 15 | `commands/ai.rs:1638` | 摘要批处理大小 |
| `VECTOR_THRESHOLD` | 0.65 | `commands/ai.rs:869` | 语义检索余弦相似度阈值 |
| `MAX_SEQ_LEN` | 512 | `ai/local_embed.rs:11` | BGE tokenizer 最大序列长度 |
| `QUERY_PREFIX` | `"为这个句子生成表示以用于检索相关文章："` | `ai/local_embed.rs:12` | BGE 提问前缀 |

### 文件映射

| 功能 | 主文件 | 行数 |
|------|--------|------|
| LLM 调用（chat/chat_stream/embed） | `ai/mod.rs` | 1001 |
| RAG 管线编排（全部逻辑） | `commands/ai.rs` | 3066 |
| 查询改写 | `ai/skills/query_rewrite.rs` | 67 |
| 范围解析 | `ai/skills/scope_resolver.rs` | 154 |
| 检索 | `ai/skills/retrieval.rs` | 132 |
| 上下文组装 | `ai/skills/context_assembly.rs` | 132 |
| RAG 管线编排器 | `ai/skills/pipeline.rs` | 121 |
| 本地嵌入 | `ai/local_embed.rs` | 178 |
| 前端聊天面板 | `components/ChatPanel.tsx` | 755 |
| 前端 AI 聊天页 | `pages/AiChat.tsx` | 765 |

---

## 内容注入与截断策略

### 三步注入流程

```
┌─────────────────────────────────────────────────────────────────────┐
│                       CONTEXT_BUDGET: 150,000 chars                  │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │ SYSTEM_OVERHEAD (2,000) │ ANSWER_RESERVE (8,000)                ││
│  └─────────────────────────────────────────────────────────────────┘│
│                                                                     │
│  ┌──────────────────────────────┐ ┌────────────────────────────────┐│
│  │ Layer 0: 引用文档             │ │ Layer 1: BM25 命中              ││
│  │ mention_budget = BUDGET / 3  │ │ content_budget = 剩余           ││
│  │ 全部注入，chunked 截断        │ │ 30 份（或全量），每份按配额截断  ││
│  └──────────────────────────────┘ └────────────────────────────────┘│
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │ Layer 2: 剩余命中摘要 (batch_summarize)                          ││
│  │ 最多 60 份，BATCH_SIZE=15, MAX_CONCURRENCY=6 并行 LLM 调用       ││
│  └─────────────────────────────────────────────────────────────────┘│
│                                                                     │
│  最终截断：truncate_text(docs.join("\n\n---\n\n"), max_context_chars) │
│    full_recall=true  → 140,000 chars                                │
│    full_recall=false → 50,000 chars                                 │
└─────────────────────────────────────────────────────────────────────┘
```

### full_recall 对注入的影响

| 场景 | Layer 0 | Layer 1 | Layer 2 | 语义扫描 | 总长度 |
|------|---------|---------|---------|---------|--------|
| full_recall=false | 全量，50k 截断 | 30 份 | 60 份摘要 | ≤500 条 | 50k |
| full_recall=true | 全量，50k 截断 | 全量 | 全量摘要 | 全量 | 140k |

**引用文档走 Layer 0，不受 full_recall 影响**——即使引用 50 份文档，全部注入 Layer 0，但受 mention_budget 截断。

### 内容遗漏的 6 种情况

| # | 原因 | 条件 | 缓解方法 |
|---|------|------|---------|
| 1 | 引用文档总内容超 50k 截断 | 引用文档内容很大 | 减少引用，聚焦章节 |
| 2 | Layer 1 只注入 30 份 | full_recall=false | 开启全量召回 |
| 3 | 每份文档按配额截断 | 命中多，单份配额少 | 缩小范围或开启全量召回 |
| 4 | 超出 60 份不做摘要 | 命中 > 90 | 缩小范围或开启全量召回 |
| 5 | 总长度截断到 50k/140k | 始终存在 | 缩小范围，聚焦关键文档 |
| 6 | 语义扫描 ≤500 条 | full_recall=false | 开启全量召回 |

---

## 事件系统

### 事件流向

```
Tauri Event Bus
    │
    ├─ emit("ai-progress") ───→ Tauri 前端 listen("ai-progress") → setProgress()
    │                          （桌面窗口有，Web 模式无）
    │
    ├─ emit("ai-chunk") ───┬─→ Tauri 前端 listen("ai-chunk") → setStreaming()
    │                      └─→ BRIDGED_EVENTS → event_tx.send() → /api/events SSE
    │
    └─ emit("ai-done") ────┬─→ Tauri 前端 listen("ai-done") → onSessionChange()
                           └─→ BRIDGED_EVENTS → event_tx.send() → /api/events SSE
```

### BRIDGED_EVENTS（Web 事件桥）

```rust
// webapi/mod.rs
const BRIDGED_EVENTS: &[&str] = &[
    "scan-progress",       // 扫描进度
    "scan-completed",      // 扫描完成
    "ai-chunk",            // AI 流式增量
    "ai-done",             // AI 回答完成
    "migration-progress",  // 迁移进度
    "migration-warning",   // 迁移警告
    "funasr-install-done", // FunASR 安装完成
    "bge-install-done",    // BGE 安装完成
    "restore-completed",   // 恢复完成
];
// 注意：ai-progress 不在 BRIDGED_EVENTS 中！
```

### 前端 loading 状态机

```
[发送前] pending_started_at = null, loading = false
  → 无 loading 指示器

[发送] patchSession({ pending_started_at: Date.now() })
  → loading = true → 显示"思考中" + 取消按钮

[ai-chunk] onChunk() → setStreaming(text: delta) → 流式文本显示

[ai-progress] onProgress() → setProgress({ phase, message }) → 阶段文本（仅 Tauri）

[ai-done] onDone()
  → loading = false → 移除"思考中"
  → 追加 assistant 消息 + saveChatSession()

[取消] handleCancel()
  → cancelAiRequest() → 保留已输出文本 + "已取消"
```

---

## Web API 服务器

### 启动条件

从 `app_settings` 表读取：`web_api_enabled=true`, `web_api_port=8443`, `web_api_bind=0.0.0.0`, `web_api_token=<uuid>`

### 认证

```rust
// auth::bearer_auth 中间件
// 请求头：Authorization: Bearer <token>
// token 从 app_settings 读取，通过 POST /api/auth/token 更新
// 浏览器模式：URL ?token=xxx 参数自动保存到 localStorage
```

### 路由注册

```rust
// routes/mod.rs
build_router(api_state)
  .merge(search::router(state))   // /api/search, /api/suggest, ...
  .merge(ai::router(state))       // /api/ai/*, /api/chat/*
  .merge(files::router(state))    // /api/files/*
  .merge(events::router(state))   // /api/events
  .merge(settings::router(state)) // /api/settings
  .merge(logs::router(state))     // /api/logs
  .merge(dirs::router(state))     // /api/dirs
  .merge(config::router(state))   // /api/config
  .merge(index::router(state))    // /api/index/*
  .merge(backup::router(state))   // /api/backup/*
  .merge(tesseract::router(state))// /api/ocr/*
  .route_layer(auth::bearer_auth)  // 所有 /api/* 需认证
  .merge(auth_free_routes)         // /api/auth/* 豁免
  .fallback(static_files::serve_static) // 前端静态文件（dist/）
```

### 完整路由表

#### AI 端点

| 方法 | 路径 | 请求体 | 响应 | 说明 |
|------|------|--------|------|------|
| GET | `/api/ai/capabilities` | — | `{ embedding, llm }` | AI 能力探测 |
| POST | `/api/ai/conversation/ask` | `{ messages, source_ids, ... }` | `{ answer }` | 非流式对话 |
| POST | `/api/ai/conversation/ask/stream` | 同上 + `session_id` | SSE触发 | 流式对话 |
| POST | `/api/ai/smart-search` | `{ query }` | `{ answer, evidence }` | 非流式搜索 |
| POST | `/api/ai/smart-search/stream` | `{ query, session_id }` | SSE触发 | 流式搜索 |
| POST | `/api/ai/summarize` | `{ file_id }` | `{ summary }` | 文档摘要 |
| POST | `/api/chat/ask` | `{ file_ids, question }` | `{ answer }` | 文件问答 |
| POST | `/api/ai/cancel` | — | 200 | 取消 |
| GET | `/api/ai/gateways/test` | — | `[{ kind, ok }]` | 网关测试 |
| GET | `/api/ai/events` | `?session_id=xxx` | `[{ ... }]` | 会话事件 |
| GET | `/api/ai/events/turn` | `?session_id=xxx&turn_number=N` | `[{ ... }]` | 单轮事件 |

#### 会话端点

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/chat/sessions` | 列出所有会话 |
| POST | `/api/chat/sessions` | 创建新会话 |
| GET | `/api/chat/sessions/{id}` | 加载会话 |
| PUT | `/api/chat/sessions/{id}` | 保存会话 |
| DELETE | `/api/chat/sessions/{id}` | 删除会话 |
| POST | `/api/chat/sessions/{id}/export` | 导出 Markdown |

#### 其他端点

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/events` | SSE 事件桥 |
| GET | `/api/search` | 全文搜索 |
| GET | `/api/settings` | 获取设置 |
| GET | `/api/config` | 获取配置 |
| GET | `/api/dirs` | 目录列表 |
| GET | `/api/index/status` | 索引状态 |
| GET | `/api/logs` | 日志 |
| GET | `/api/stats/file-types` | 文件类型统计 |
| GET | `/api/backup/status` | 备份状态 |
| GET | `/api/ocr/dependencies` | OCR 依赖 |
| GET | `/api/files/{id}/preview` | 文件预览 |
| POST | `/api/auth/token` | 更新 token |

---

## 前端架构

### 路由结构

```
/                    → SearchPage    # 搜索页
/#/chat              → AiChat        # AI 聊天页
/#/browse            → Browse        # 浏览页
/#/directories       → Directories   # 资料库
/#/index             → IndexStatus   # 索引状态
/#/logs              → Logs          # 日志
/#/file-types        → FileTypes     # 文件类型
/#/settings          → Settings      # 设置
```

### AI 聊天组件树

```
AiChat
├── 会话列表（左侧）
│   ├── 搜索框 + 筛选按钮（全部/今日/7天/更早）
│   ├── 会话条目列表（点击切换）
│   └── 文件树浏览器（懒加载，右键加入范围）
│
├── ChatPanel（右侧）
│   ├── 标题栏（导出按钮）
│   ├── 消息列表
│   │   ├── 用户消息（Markdown 渲染）
│   │   ├── 助手消息（Markdown 渲染 + 引用高亮）
│   │   ├── 检索依据面板（AiEvidencePanel）
│   │   ├── 推理过程时间线（AiEventTimeline）
│   │   └── loading 指示器（思考中 + 取消按钮）
│   ├── 范围条（chips：📄 文件 / 📁 目录，可 × 删除）
│   ├── 开关栏（仅依据文档 / 全量召回）
│   └── 输入框 + 发送按钮
│
└── 状态栏（文件数、已索引、错误、备份）
```

### 双模式 API 客户端

```typescript
// client.ts
export async function invoke<T>(cmd: string, args?: InvokeArgs): Promise<T> {
  if (isTauri()) {
    // 桌面模式：Tauri IPC
    const { invoke: tauriInvoke } = await import('@tauri-apps/api/core')
    return tauriInvoke<T>(cmd, args)
  }
  // 浏览器模式：HTTP fetch
  const spec = MAPPINGS[cmd]
  // ... fetch + transform + SSE 处理
}
```

### 平台检测

```typescript
// utils/platform.ts
export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}
export function getApiBase(): string {
  return typeof window !== 'undefined' ? window.location.origin : ''
}
export function getToken(): string {
  return localStorage.getItem('ls_token') || ''
}
```

---

## 配置系统

### 配置文件位置

```
桌面模式：~/Library/Application Support/.link-searcher/config.json
覆盖模式：LINK_SEARCHER_DATA_DIR 环境变量
```

### 配置结构

```json
{
  "data_dir": "/Volumes/Data/index",
  "language": "zh",
  "providers": [
    {
      "id": "95269265-fe6b-4212-a496-5ebe037c6a77",
      "name": "9router",
      "base_url": "http://192.168.1.50:20128/v1",
      "api_key": "sk-xxx",
      "models": [{ "id": "coding", "model_type": "Llm" }]
    }
  ],
  "active_embedding_model_id": "local:bge-large-zh-v1.5",
  "active_llm_model_id": "95269265-fe6b-4212-a496-5ebe037c6a77:coding",
  "semantic_weight": 0.3
}
```

- `active_embedding_model_id` 格式：`"local:bge-large-zh-v1.5"` 或 `"provider_id:model_id"`
- `local:` 前缀的 embedding 模型使用本地 BGE ONNX 引擎
- `active_llm_model_id` 格式：`"provider_id:model_id"`

---

## 构建与部署

```bash
# 开发模式
npm install && npm run tauri dev

# 仅前端 Vite
npm run dev

# 仅编译后端
cargo build --manifest-path src-tauri/Cargo.toml

# 生产构建
npm run tauri build
# 产物：src-tauri/target/release/bundle/

# CLI 模式
./target/debug/link-searcher chat "判决结果是什么？" --dry-run

# 指定数据目录
LINK_SEARCHER_DATA_DIR=/Volumes/Data/index ./target/debug/link-searcher chat "测试"
```

### 环境变量

| 变量 | 说明 |
|------|------|
| `LINK_SEARCHER_DATA_DIR` | 覆盖数据目录 |
| `SHERPA_ONNX_ARCHIVE_DIR` | sherpa-onnx 预编译库路径 |
| `SHERPA_ONNX_LIB_DIR` | sherpa-onnx 库解压路径 |

---

## 测试

```bash
# Rust 测试
cd src-tauri && cargo test

# E2E 测试（37 个用例，覆盖 8 页面 + AI 流式）
# 详见 docs/12-testing.md

# 性能测试
python3 scripts/gen_test_data.py /tmp/ls-test-1k 1000
./scripts/perf_scan.sh /tmp/ls-test-1k 1k-files

# 静态分析
semgrep scan --config .semgrep/custom.yml --config p/owasp-top-ten --config p/secrets --severity ERROR
```

---

## 调试与诊断

### 后端日志

```bash
# 实时监控
tail -f /Volumes/Data/index/app.log

# AI 请求完整流程
grep "conversation_ask_stream\|chat_stream returned\|turn_end" app.log

# 内容注入详情
grep "injection:\|context assembled\|prepare ok\|prepare failed" app.log

# LLM 调用
grep "chat stream request\|← LLM\|→ LLM" app.log

# 事件桥
grep "WEBAPI-BRIDGE" app.log

# 原始回答内容
grep "raw answer" app.log
```

### 前端调试

```javascript
// ChatPanel.tsx 已埋入 AI-DEBUG 日志
console.log('[AI-DEBUG] ai-chunk', { deltaLen, isReasoning })
console.log('[AI-DEBUG] ai-done received', { sessionId, loadingRef, fullTextLen })
console.warn('[AI-DEBUG] ai-done dropped: loadingRef is false')
```

### CLI 测试

```bash
link-searcher chat "问题" --no-llm    # 仅检索
link-searcher chat "问题" --dry-run   # 完整管线不调 LLM
link-searcher chat "问题" --full-recall --dry-run  # 全量召回
link-searcher chat "问题" --scope "案件/xxx" --dry-run  # 指定范围
```

### 直接测试 LLM 网关

```bash
curl -s http://192.168.1.50:20128/v1/chat/completions \
  -H "Authorization: Bearer <key>" \
  -d '{"model":"coding","messages":[{"role":"user","content":"hi"}],"max_tokens":20}'
```

### 直接测试 Web API

```bash
# 非流式
curl -sk https://127.0.0.1:8443/api/ai/conversation/ask \
  -H "Authorization: Bearer 123456" \
  -d '{"messages":[{"role":"user","content":"测试"}],"source_ids":[],"strict_docs":false}'

# 流式 + 事件桥
curl -sk "https://127.0.0.1:8443/api/events" -H "Authorization: Bearer 123456" &
curl -sk -X POST "https://127.0.0.1:8443/api/ai/conversation/ask/stream" \
  -H "Authorization: Bearer 123456" \
  -d '{"messages":[{"role":"user","content":"测试"}],"source_ids":[],"sessionId":"test","strict_docs":false}'
```

### 查看数据库

```bash
# AI 事件
sqlite3 /Volumes/Data/index/data.db \
  "SELECT turn_number, event_seq, event_type, substr(payload_json,1,200)
   FROM ai_events WHERE session_id='<id>' ORDER BY id;"

# 会话内容
cat /Volumes/Data/index/chat_history.json | python3 -m json.tool | head -100
```

---

## 已知缺陷与设计约束

### 1. 内容截断无位置感知
`truncate_text` 从开头截断，可能丢掉文档末尾的关键信息。无分块重排机制。

### 2. `content_budget` 过度压缩
命中多时每份文档配额极低（200 份时每份仅 700 chars），无法有效利用上下文。

### 3. `ai-done` 被 `loadingRef` 拦截
`ChatPanel.tsx:166` 检查 `loadingRef.current`，若时序问题被清 false，回复静默丢弃。

### 4. `ai-progress` 不在 Web 事件桥中
Web 模式下前端不显示进度条和阶段文本。

### 5. `vector_full_scan` 全表扫描
无索引，遍历 11684 条嵌入约需 10-30 秒。

### 6. 上下文窗口不可配置
`CONTEXT_BUDGET=150000` 硬编码。

### 7. 查询改写不感知检索范围
`llm_rewrite_query` 不接收 scope 信息，可能改写后丢失范围限定。

### 8. 会话存储无版本迁移
`chat_history.json` 无 schema 版本，依赖 serde `default` 兜底。

### 9. 前端浏览器模式 token 竞态
首次 API 请求无 token 导致 401。

### 10. `chat_stream` 无超时控制
SSE 流式读取无全局超时，网关挂起则永久阻塞。

### 11. Web API 单进程
`axum_server` 在 Tauri 主进程内运行，LLM 调用阻塞可能影响 GUI 响应。

### 12. 语义检索仅用余弦相似度
`vector_full_scan` 不带 top-K，返回所有 ≥threshold 的文档可能造成噪声。

---

*最后更新：2026-08-31*
```
