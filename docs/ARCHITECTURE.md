# Link-Searcher 开发文档

> 面向开发者的架构、核心流程、调试指南。适用于 AI RAG 管线、全文搜索、桌面应用开发。

---

## 目录

1. [技术栈](#技术栈)
2. [项目结构](#项目结构)
3. [AI RAG 管线架构](#ai-rag-管线架构)
4. [核心数据结构](#核心数据结构)
5. [请求路径](#请求路径)
6. [内容注入与截断策略](#内容注入与截断策略)
7. [事件系统](#事件系统)
8. [Web API 服务器](#web-api-服务器)
9. [调试与诊断](#调试与诊断)
10. [已知缺陷与设计约束](#已知缺陷与设计约束)

---

## 技术栈

| 层 | 技术 |
|-----|------|
| 桌面框架 | Tauri 2.x（Rust + WebView） |
| 前端 | React 19 + TypeScript + Tailwind CSS 4 |
| 全文搜索 | Tantivy 0.22 + jieba-rs |
| 向量检索 | BGE-large-zh-v1.5（tract ONNX 本地推理） |
| LLM 网关 | OpenAI 兼容 API（Ollama / OneAPI / vLLM） |
| 数据库 | SQLite（rusqlite + r2d2 连接池） |
| 异步运行时 | tokio（多线程） |
| 并行处理 | Rayon（文本提取） |
| HTTP 服务 | axum + axum-server（TLS） |

---

## 项目结构

```
src-tauri/src/
├── main.rs              # 入口：GUI 或 CLI 分发
├── lib.rs               # Tauri 启动 + Web API 启动
├── cli.rs               # 命令行接口（search, chat, scan, watch, health）
├── config.rs            # 配置管理（AI 端点、目录、语义权重）
├── state.rs             # AppState 全局状态
├── boot.rs              # 核心组件初始化（DB、索引、扫描器）
├── indexer.rs           # 批量索引 + 流式 MD5 + 去重
├── search/              # Tantivy 搜索引擎
│   ├── mod.rs           # IndexManager（reader 缓存）
│   ├── schema.rs        # 字段定义 + jieba tokenizer
│   ├── indexer.rs       # 文档增删
│   └── searcher.rs      # 搜索/建议/导出
├── db/                  # SQLite 数据库
│   ├── tracker.rs       # 文件追踪 CRUD + 统计
│   ├── chunks.rs        # 分块存储
│   ├── ai_events.rs     # AI 事件记录
│   └── ...
├── extractor/           # 文本提取（PDF/Office/图片/音频/OCR）
├── scanner/             # 目录扫描 + 文件监控
├── ai/                  # AI 核心模块
│   ├── mod.rs           # LLM 调用（chat/chat_stream/embed/vector_full_scan）
│   ├── local_embed.rs   # 本地 BGE 嵌入引擎
│   └── skills/          # RAG 管线 Skills
│       ├── pipeline.rs         # RAGPipeline 编排
│       ├── query_rewrite.rs    # 查询改写（规则 + LLM）
│       ├── scope_resolver.rs   # 检索范围解析
│       ├── retrieval.rs        # BM25 + 语义检索 + 来源管理
│       └── context_assembly.rs # 上下文组装
├── commands/            # Tauri IPC 命令
│   ├── ai.rs            # AI 对话/问答/摘要（3066 行）
│   ├── search.rs        # 搜索
│   ├── files.rs         # 文件操作
│   └── ...
└── webapi/              # 可选 HTTPS Web API
    ├── mod.rs           # 服务器启动 + 事件桥
    ├── auth.rs          # Bearer token 认证
    ├── routes/          # API 路由
    │   ├── ai.rs        # AI 端点
    │   ├── events.rs    # SSE 事件桥
    │   └── ...
    └── static_files.rs  # 前端静态文件（dist/）
```

### 前端结构

```
src/
├── api/
│   ├── client.ts        # 统一 API 客户端（Tauri IPC / HTTP fetch）
│   ├── files.ts         # 文件/会话/流式 API
│   └── search.ts        # 搜索 API
├── components/
│   ├── ChatPanel.tsx    # AI 聊天面板（755 行）
│   ├── AiEventTimeline.tsx  # AI 推理时间线
│   └── ...
├── pages/
│   ├── AiChat.tsx       # AI 聊天页（会话管理）
│   ├── SearchPage.tsx   # 搜索页
│   └── ...
├── hooks/
├── i18n/
└── utils/
    └── platform.ts      # 平台检测（Tauri vs 浏览器）
```

---

## AI RAG 管线架构

### 概览

```
用户输入 "判决结果是什么？"
        │
        ▼
┌──────────────────────────────────────────────┐
│  QueryRewrite                                │
│  规则改写（指代消解） → LLM 改写（可选）        │
│  "判决结果是什么？" → "判决结果"                 │
└──────────────────────┬───────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────┐
│  ScopeResolver                               │
│  解析 @mention / 目录 / 范围 / 条件命令         │
│  → dir_ids, path_prefixes, file_ids, ext      │
└──────────────────────┬───────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────┐
│  Retrieval（三路合并）                         │
│  ① BM25 全文检索（Tantivy）                    │
│  ② 语义向量全量扫描（BGE + cosine）              │
│  ③ SQL 路径匹配（文件名关键词）                  │
│  三路合并去重 → all_hits                       │
└──────────────────────┬───────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────┐
│  ContextAssembly（三层注入）                    │
│  Layer 0: 引用文档（@mention）全部注入          │
│  Layer 1: BM25 命中（最多 30/全量）            │
│  Layer 2: 剩余命中 → batch_summarize 摘要      │
│  → system prompt + user_msg                   │
└──────────────────────┬───────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────┐
│  LLM 调用（chat_stream）                       │
│  OpenAI 兼容 API → 流式 SSE 响应               │
│  → ai-chunk 逐片 emit → ai-done 最终 emit     │
└──────────────────────────────────────────────┘
```

### 关键常量

| 常量 | 值 | 说明 |
|------|-----|------|
| `CONTEXT_BUDGET` | 150,000 chars | 总上下文预算 |
| `SYSTEM_OVERHEAD` | 2,000 chars | system prompt 模板开销 |
| `ANSWER_RESERVE` | 8,000 chars | 为 LLM 回答预留空间 |
| `MAX_CONTENT_INJECT` | 30 | 默认 Layer 1 注入上限 |
| `MAX_REMAINING_SUMMARY` | 60 | Layer 2 摘要上限 |
| `MAX_CONCURRENCY` | 6 | batch_summarize 并行 LLM 调用数 |
| `VECTOR_THRESHOLD` | 0.65 | 语义检索余弦相似度阈值 |
| `BATCH_SIZE` | 15 | 摘要批处理大小 |

### 文件映射

| 功能 | 主文件 | 行数 |
|------|--------|------|
| LLM 调用（chat/chat_stream/embed） | `ai/mod.rs` | 1001 |
| RAG 管线编排 | `commands/ai.rs` | 3066 |
| 查询改写 | `ai/skills/query_rewrite.rs` | 67 |
| 范围解析 | `ai/skills/scope_resolver.rs` | 154 |
| 检索 | `ai/skills/retrieval.rs` | 132 |
| 上下文组装 | `ai/skills/context_assembly.rs` | 132 |
| 本地嵌入 | `ai/local_embed.rs` | 178 |
| 前端聊天面板 | `components/ChatPanel.tsx` | 755 |
| 前端 AI 聊天页 | `pages/AiChat.tsx` | 765 |

---

## 核心数据结构

### conversation_ask_stream 请求

```rust
// 前端 → 后端
pub async fn conversation_ask_stream(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    messages: Vec<ChatMessage>,       // 多轮对话历史
    source_ids: Vec<String>,          // 上一轮检索到的文件 ID
    session_id: String,
    scope: TurnScope,                 // 本轮检索范围
    session_retrieval_scope: Vec<String>, // 跨轮累计范围
    strict_docs: bool,                // 仅依据文档
    full_recall: Option<bool>,        // 全量召回
) -> Result<(), String>
```

### 事件流

```rust
// 后端 emit 事件
AiProgress { session_id, phase, message, current, total }  // "ai-progress"
AiChunk { session_id, delta, reasoning }                    // "ai-chunk"
AiDone { session_id, full_text, took_ms, cancelled, ... }   // "ai-done"

// 前端监听
listenAiStream(sessionId, onChunk, onDone)
listenAiProgress(sessionId, onProgress)
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
```

---

## 请求路径

项目支持三种请求路径，前端根据 `isTauri()` 自动切换：

### 路径 1：Tauri IPC（桌面窗口）

```
前端 ChatPanel.handleSend()
  → client.invoke('conversation_ask_stream')
  → Tauri IPC → #[tauri::command] conversation_ask_stream()
  → emit("ai-chunk") / emit("ai-done") → Tauri event bus
  → 前端 listenAiStream() 接收
```

### 路径 2：Web API + SSE 事件桥（浏览器）

```
前端 ChatPanel.handleSend()
  → client.invoke(...) → HTTP POST /api/ai/conversation/ask/stream
  → axum handler → spawn(conversation_ask_stream())
  → emit("ai-chunk") → Tauri event bus → BRIDGED_EVENTS
  → event_tx.send() → /api/events SSE stream
  → 前端 listen() 接收
```

### 路径 3：CLI（命令行）

```
link-searcher chat "判决结果是什么？"
  → cli.rs → prepare_conversation_prompt()
  → chat_stream() → 打印到 stdout
```

---

## 内容注入与截断策略

AI 问答将文档内容注入 LLM 时分三层，每层有独立的截断策略：

### Layer 0：引用文档（@mention）

```rust
// 全部注入，不受 MAX_CONTENT_INJECT 限制
for (fid, resolved_path) in mention_resolved.iter() {
    docs.push(format!("[{n}]（{resolved_path}）\n{}", chunked_or_truncated(...)));
}
```

- 分配 `mention_budget = CONTEXT_BUDGET / 3`（约 50k chars）
- 超出预算被截断，**不保证全文进入 LLM**

### Layer 1：BM25 命中

```rust
let inject_limit = if full_recall { all_hits.len().max(1) } else { MAX_CONTENT_INJECT }; // 30
let content_hits = all_hits.iter()
    .filter(|h| !mention_index.contains_key(&h.path))  // 排除已引用
    .take(inject_limit);
```

- 每份文档分配 `content_budget / content_hits.len()` 字符
- 命中 30 份时每份约 4666 chars
- **`full_recall=true` 消除 30 份上限，但每份配额更少**

### Layer 2：剩余命中摘要

```rust
let remaining_hits = all_hits.iter()
    .filter(|h| !injected_ids.contains(&h.file_id))
    .take(MAX_REMAINING_SUMMARY);  // 60
batch_summarize(state, &remaining_ids, &query, ...)
```

- 最多 60 份走摘要，其余丢弃
- 摘要并行度：`MAX_CONCURRENCY=6`，`BATCH_SIZE=15`

### 最终截断

```rust
let max_context_chars = if full_recall { 140000 } else { 50000 };
let context = truncate_text(&docs.join("\n\n---\n\n"), max_context_chars);
```

### 总结

| 条件 | Layer 0 | Layer 1 | Layer 2 | 总长度 | 遗漏风险 |
|------|---------|---------|---------|--------|---------|
| 无引用 + full_recall=false | N/A | 30 份 | 60 份摘要 | 50k | 高 |
| 无引用 + full_recall=true | N/A | 全量 | 全量摘要 | 140k | 中 |
| 有引用 + full_recall=false | 全量 | 30 份额外 | 60 份摘要 | 50k | 中 |
| 有引用 + full_recall=true | 全量 | 全量额外 | 全量摘要 | 140k | 低 |

---

## 事件系统

### 事件桥（Web API 模式）

```rust
// webapi/mod.rs
const BRIDGED_EVENTS: &[&str] = &[
    "scan-progress", "scan-completed",
    "ai-chunk", "ai-done",
    "migration-progress", "migration-warning",
    "funasr-install-done", "bge-install-done",
    "restore-completed",
];

// 注意：ai-progress 不在 BRIDGED_EVENTS 中！
// Web 模式下前端不显示进度条和阶段文本。
```

### 前端 loading 状态机

```
用户点击发送
  → pending_started_at = Date.now()  (loading=true, 显示"思考中")
  → setStreaming({ text:'', reasoning:'' })
  → conversationAskStream()  // 异步

ai-chunk 到达
  → setStreaming({ text: delta, ... })  (流式显示)

ai-progress 到达
  → setProgress(...)  (显示阶段文本，如"BM25 检索中...")

ai-done 到达
  → if !loadingRef.current → 丢弃！（已知坑）
  → 追加 assistant 消息
  → pending_started_at = null  (loading=false)
  → saveChatSession()
```

**已知问题**：`ai-done` 回调检查 `loadingRef.current`，若它已被其他 effect 清为 false，回复**静默丢弃**。这解释了"思考中消失 + 无回复"的现象。

---

## Web API 服务器

### 启动条件

```rust
// 从 app_settings 读取
web_api_enabled: true
web_api_port: 8443
web_api_bind: 0.0.0.0
web_api_token: <uuid>
```

### 认证

- Bearer token 中间件（`auth::bearer_auth`）应用于所有 `/api/*` 路由
- token 从 `app_settings` 表读取，可通过 `POST /api/auth/token` 更新
- 浏览器模式：前端 URL `?token=xxx` 参数自动保存到 localStorage

### 路由一览

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/search` | 搜索 |
| GET | `/api/ai/capabilities` | AI 能力探测 |
| POST | `/api/ai/conversation/ask` | 非流式 AI 对话 |
| POST | `/api/ai/conversation/ask/stream` | 流式 AI 对话（SSE 触发） |
| GET | `/api/events` | SSE 事件桥（接收 ai-chunk/ai-done） |
| GET/POST | `/api/chat/sessions` | 会话 CRUD |
| POST | `/api/ai/smart-search/stream` | 流式智能搜索 |
| POST | `/api/ai/summarize` | 文档摘要 |
| GET | `/api/ai/gateways/test` | 网关测试 |
| POST | `/api/ai/cancel` | 取消 AI 请求 |
| GET | `/api/stats/file-types` | 文件类型统计 |
| GET | `/api/settings` | 设置 |
| GET | `/api/config` | 配置 |
| GET | `/api/dirs` | 目录列表 |
| GET | `/api/index/status` | 索引状态 |
| GET | `/api/backup/status` | 备份状态 |
| GET | `/api/ocr/dependencies` | OCR 依赖 |
| GET | `/api/files/:id/preview` | 文件预览 |
| GET | `/api/files/:id/raw` | 文件原始内容 |
| GET | `/api/logs` | 日志 |
| `/*` | fallback | 静态文件（dist/） |

### SSE 事件桥

```
Tauri event bus
    │
    ├─ listen_any("ai-chunk") → event_tx.send(("ai-chunk", payload))
    ├─ listen_any("ai-done")  → event_tx.send(("ai-done", payload))
    └─ ...
         │
         ▼
    broadcast::channel(256)
         │
         ▼
    /api/events (SSE) ──→ 前端 fetch + ReadableStream
```

---

## 调试与诊断

### 后端日志

日志文件：`data_dir/app.log`（默认 `~/Library/Application Support/.link-searcher/app.log`）

关键日志模式：

```bash
# AI 请求流程
grep "conversation_ask_stream\|▶ stream\|chat_stream returned\|turn_end" app.log

# 内容注入
grep "injection:\|context assembled\|prepare ok\|prepare failed" app.log

# LLM 调用
grep "chat stream request\|chat stream response\|← LLM\|→ LLM" app.log

# 事件桥
grep "WEBAPI-BRIDGE" app.log

# 原始回答内容
grep "raw answer" app.log
```

### 前端调试

在 `ChatPanel.tsx` 中已埋入 `[AI-DEBUG]` 日志：

```javascript
console.log('[AI-DEBUG] ai-chunk', { deltaLen, isReasoning })
console.log('[AI-DEBUG] ai-done received', { sessionId, loadingRef, fullTextLen })
console.warn('[AI-DEBUG] ai-done dropped: loadingRef is false')
```

### CLI 测试

```bash
# 仅检索（不调用 LLM）
link-searcher chat "判决结果是什么？" --no-llm

# 完整管线（不调用 LLM，显示注入详情）
link-searcher chat "判决结果是什么？" --dry-run

# 全量召回
link-searcher chat "判决结果是什么？" --full-recall --dry-run
```

### 直接测试 LLM 网关

```bash
curl -s http://192.168.1.50:20128/v1/chat/completions \
  -H "Authorization: Bearer <key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"coding","messages":[{"role":"user","content":"hi"}],"max_tokens":20}'
```

### 直接测试 Web API

```bash
# 非流式
curl -sk https://127.0.0.1:8443/api/ai/conversation/ask \
  -H "Authorization: Bearer 123456" \
  -H 'Content-Type: application/json' \
  -d '{"messages":[{"role":"user","content":"测试"}],"source_ids":[],"strict_docs":false}'

# 流式 + 事件桥
curl -sk "https://127.0.0.1:8443/api/events" -H "Authorization: Bearer 123456" &
curl -sk -X POST "https://127.0.0.1:8443/api/ai/conversation/ask/stream" \
  -H "Authorization: Bearer 123456" -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"测试"}],"source_ids":[],"sessionId":"test","strict_docs":false}'
```

### 查看 AI 事件

```sql
SELECT turn_number, event_seq, event_type, substr(payload_json,1,200)
FROM ai_events
WHERE session_id = '<session_id>'
ORDER BY id;
```

---

## 已知缺陷与设计约束

### 1. `ai-progress` 不在 Web 事件桥中

`ai-progress` 事件未加入 `BRIDGED_EVENTS`，Web 模式下前端收不到进度信息（不显示"BM25 检索中..."等阶段文本）。

### 2. `ai-done` 被 `loadingRef` 拦截

`ChatPanel.tsx:166` 中 `ai-done` 回调检查 `loadingRef.current`，若因时序问题已被清为 false，回复静默丢弃。表现为"思考中消失 + 无回复"。

### 3. `content_budget` 过度压缩

Layer 1 每份文档注入分配 `content_budget / content_hits.len()` 字符。命中 30 份时每份约 4666 chars，命中 200 份时每份仅 700 chars——无法有效利用上下文。

### 4. `batch_summarize` 无速率限制

并行 LLM 调用的 `MAX_CONCURRENCY=6` 固定，但 `remaining_hits` 曾无上限（已修复为 `MAX_REMAINING_SUMMARY=60`）。

### 5. 内容截断无位置感知

`truncate_text` 从开头截断，可能丢掉文档末尾的关键信息（如判决书结论）。无分块重排（chunk reranking）机制。

### 6. 上下文窗口不可配置

`CONTEXT_BUDGET=150000` 和 `max_context_chars=50000/140000` 硬编码，不支持按模型上下文窗口调整。

### 7. 查询改写不感知检索范围

LLM 查询改写（`llm_rewrite_query`）不接收 `mention_resolved` 或 `scope` 信息，可能改写后丢失范围限定。

### 8. 语义检索仅用余弦相似度

`vector_full_scan` 不带 top-K 截断，返回所有 `≥threshold` 的文档，可能造成大量语义噪声。

### 9. 前端浏览器模式 token 竞态

`App.tsx` 中 `useEffect` 设置 `setToken(urlToken)` 与初始渲染存在竞态，首次 API 请求无 token 导致 401，触发 `auth-failed` 事件。

### 10. 会话存储无版本迁移

`chat_history.json` 无 schema 版本，字段变更（如 `per_turn_evidence`）依赖 serde `default` 兜底，向前兼容性靠运气。

---

*最后更新：2026-08-31*