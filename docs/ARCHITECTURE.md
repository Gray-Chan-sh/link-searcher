# Link-Searcher 设计手册

> 分模块讲解 Link-Searcher 的设计思想、架构决策与实现目标。
> 面向希望理解"为什么这样设计"以及"如何复现一个类似程序"的开发者。
> 每一模块说明：**它解决什么问题 → 为什么这样设计 → 核心架构 → 关键决策 → 边界与取舍**。

---

## 目录

- [一、总体设计思想](#一总体设计思想)
- [二、模块一：全文搜索引擎（Tantivy + jieba）](#二模块一全文搜索引擎tantivy--jieba)
- [三、模块二：文本提取管线（Extractor）](#三模块二文本提取管线extractor)
- [四、模块三：目录扫描与文件监控（Scanner）](#四模块三目录扫描与文件监控scanner)
- [五、模块四：AI 出口（LLM 网关调用）](#五模块四ai-出口llm-网关调用)
- [六、模块五：语义向量检索（BGE 本地嵌入）](#六模块五语义向量检索bge-本地嵌入)
- [七、模块六：AI 聊天与 RAG 管线](#七模块六ai-聊天与-rag-管线)
- [八、模块七：会话与事件系统](#八模块七会话与事件系统)
- [九、模块八：Web API 服务器（远程访问）](#九模块八web-api-服务器远程访问)
- [十、模块九：数据存储（SQLite）](#十模块九数据存储sqlite)
- [十一、模块十：前端架构](#十一模块十前端架构)
- [十二、模块间数据流总览](#十二模块间数据流总览)
- [十三、核心设计原则](#十三核心设计原则)
- [十四、已知取舍与未来方向〕](#十四已知取舍与未来方向)

---

## 一、总体设计思想

### 这个程序是什么

Link-Searcher 是一个**本地全文搜索 + AI 文档问答**的桌面应用。核心使命可以概括为一句话：

> **把你硬盘里的所有文档，变成一个"能全文检索、能直接提问"的私有知识库。**

### 三个核心问题

任何"本地文档智能搜索"程序都要回答三个问题：

| 问题 | Link-Searcher 的回答 |
|------|---------------------|
| **1. 文件内容从哪来？** | 一个统一的**文本提取管线**，把 PDF/Word/图片/音频等 20+ 格式统一转成纯文本 |
| **2. 怎么搜索？** | 全文搜索引擎（关键词）+ 语义向量引擎（意思）**双路融合** |
| **3. 怎么回答？** | RAG 管线：检索相关文档 → 组装上下文 → 交给 LLM 回答 |

### 设计的两条主线

1. **性能优先**：索引和搜索要快（毫秒级），所以用 Tantivy（Rust 原生全文引擎）+ Rayon 并行。
2. **隐私可选**：核心搜索完全本地，AI 是可选项。用本地 BGE 做语义搜索不出机器；用远程 LLM 才把内容发出去（用户知情）。

### 为什么用 Rust + Tauri

| 选择 | 原因 |
|------|------|
| Rust | 性能、内存安全、无 GC，适合文本处理与并发 |
| Tauri | 用 Web 技术（React）写界面，但运行时是原生性能，比 Electron 轻量得多 |
| SQLite | 零配置、单文件、稳定，够用且不过度设计 |

### 复现路线图（七步）

```
第 1 步：打通文本提取（任意文件 → 纯文本）
第 2 步：接入全文搜索索引（Tantivy）
第 3 步：目录扫描 + 文件监控（增量更新）
第 4 步：接入 LLM（chat / chat_stream）
第 5 步：语义向量检索（BGE）
第 6 步：组装 RAG 聊天（检索 + 上下文 + 回答）
第 7 步：加 Web API + 共享前端
```

---

## 二、模块一：全文搜索引擎（Tantivy + jieba）

### 解决什么问题

用户输入关键词，要能在**几毫秒内**从几十万个文件中找出相关文件。这是整个软件的地基。

### 为什么用 Tantivy 而非别家

- **Lucene 系太多依赖**，Tantivy 是纯 Rust，一条 `cargo add` 就进来。
- 差补：Tantivy 是"Rust 版的 Elasticsearch 核心"，自带索引、分片、查询语法（`AND`/`OR`/`NOT`/通配符）。
- 中文搜索需要分词：jieba-rs（结巴分词）负责把"判决结果是什么"切成 `["判决", "结果", "什么"]`。

### 核心架构

```
┌─────────────────────────────────────────────────────────┐
│  IndexManager（search/mod.rs）                           │
│  持有 Reader（读） + Writer（写），启动时打开/创建索引      │
└────────────────────────┬────────────────────────────────┘
                         │
        ┌────────────────┴────────────────┐
        │                                 │
┌───────▼────────┐                ┌───────▼────────┐
│  Schema        │                │  Searcher       │
│  （字段定义）    │                │  （查询逻辑）     │
│  content: Text │                │  BM25 排序       │
│  path: Text    │                │  模糊容错        │
│  mtime: Int    │                │  搜索建议        │
└───────┬────────┘                └───────┬────────┘
        │                                 │
        └──────────────┬──────────────────┘
                       ▼
              jieba 中文分词器
              （注册到 schema 的 content 字段）
```

### 关键设计决策

1. **字段最小化**：只索引 `content`（正文）+ `path`（文件名）。够了就不过度加字段，索引更小更快。
2. **中文分词**：在 schema 里用 `TokenizerManager` 注册 `jieba`，这样查询和写入用同一套分词，保证命中率。
3. **容错距离 1**：用户打错一个字也能匹配（模糊搜索）。
4. **Reader 缓存**：搜索频繁，Reader 复用一个，避免反复打开磁盘索引。

### 复现要点

```rust
// 1. 定义 schema
let mut schema = Schema::builder();
schema.add_text_field("content", TEXT | STORED);  // 正文，参与分词
schema.add_text_field("path", TEXT | STORED);      // 文件名
schema.add_i64_field("mtime", INDEXED);            // 时间排序

// 2. 注册中文分词
let mut tok = TokenizerManager::default();
tok.register("jieba", JiebaTokenizer::default());
schema.register_tokenizer... // 让 content 字段用 jieba

// 3. 写入
let mut index = Index::create_in_dir(&dir, schema);
let mut writer = index.writer(50_000_000)?;
writer.add_document(doc);
writer.commit();

// 4. 查询
let searcher = index.reader()?.searcher();
let query = QueryParser::for_index(&index, vec![content, path])
    .parse_query(&user_input)?;
let top = searcher.search(&query, &TopDocs::with_limit(20))?;
```

### 边界与取舍

- 只索引文本，**不索引二进制**（视频/图片等靠提取管线先转文本）。
- 大文件（>100MB）做 MD5 时只读首尾 1MB，避免整文件读入内存。
- 排序默认按相关性（BM25），可选按时间/大小。

---

## 三、模块二：文本提取管线（Extractor）

### 解决什么问题

搜索引擎只认文本。但硬盘里有 PDF、Word、Excel、PPT、图片、音频、压缩包……需要一个统一的"任意文件 → 纯文本"转换器。

### 核心架构：格式路由

```
                    extractor/mod.rs（总路由）
                           │
        根据文件扩展名分发到对应提取器
                           │
   ┌────────┬────────┬─────┴─────┬────────┬────────┐
   ▼        ▼        ▼          ▼        ▼        ▼
  pdf.rs  office/  image.rs    audio.rs  text.rs  archive.rs
           (Word/
           Excel/PPT)
```

### 每个提取器的设计

| 提取器 | 处理格式 | 核心工具 | 设计要点 |
|--------|---------|---------|---------|
| `pdf.rs` | PDF | lopdf | 能抽文本就直接抽；扫描件（不能抽文本的）走 OCR |
| `office/` | .doc/.docx/.xls/.ppt | rwml / calamine / anydoc | **纯 Rust 解析，零外部依赖**（不给用户安装 Office 的负担） |
| `image.rs` | 图片 | PaddleOCR | 图片不做别的，就是 OCR 识文字 |
| `audio.rs` | 音频 | FunASR-Nano | 语音转文字 + 说话人分离（法庭录音场景） |
| `text.rs` | 纯文本/代码 | 直接读 | 最直接，兜底格式 |
| `archive.rs` | 压缩包 | zip/tar | 枚举条目，文本直接读，Office/PDF/图片走管线 |

### 关键设计决策

1. **一个输入，多个输出标**：提取结果除了文本，还会记录 `md5`（内容哈希）、`char_count`、`ocr_used`（是否用过 OCR）。
2. **OCR 多引擎降级**：PaddleOCR → Apple Vision → Windows OCR → Tesseract，按可用性自动降级。
3. **内容去重**：相同的 `md5` 只提取一次，后续文件直接复用，省时省资源。
4. **仅存文本不存源文件**：数据库只存提取出的文本，不复制原文件，节省空间。

### 复现要点

```rust
pub fn extract(path: &Path) -> ExtractResult {
    match ext(path) {
        "pdf" => extract_pdf(path),
        "docx" | "docx" => extract_office(path),
        "png" | "jpg" => extract_image_path(path),
        "mp3" | "wav" => extract_audio(path),
        _ => extract_text(path),  // 兜底
    }
}
// 每个提取器返回文本 + 元数据，统一年纪
```

### 边界与取舍

- 未知格式也尝试纯文本兜底，尽量不丢文件。
- 支持格式清单是**白名单优先 + 黑名单排除**（`#`开头、`.`开头隐藏文件等天然排除）。

---

## 四、模块三：目录扫描与文件监控（Scanner）

### 解决什么问题

文件是**活的**——会被新增、修改、删除。索引必须跟着变，否则搜索到的是过期数据。

### 两种更新策略

1. **全量扫描**：启动或手动触发，遍历整个目录，比对 mtime。
2. **增量扫描**：只处理 `mtime` 变化或未索引的文件，秒级完成。
3. **实时监控**：`notify` 监听文件系统事件，300ms 防抖后增量更新。

### 核心架构

```
┌─────────────────────────────────────────────────────┐
│  Scanner（scanner/mod.rs）                           │
│  全量扫描 / 增量扫描 / 启动扫描                      │
│  每扫描一个文件：                                   │
│    ① 排除规则过滤（隐藏文件、临时文件、glob）         │
│    ② 查 DB：mtime 变过吗？索引过吗？                 │
│    ③ 需要 → 交给 Indexer 提取 + 索引                 │
└─────────────────────────────────────────────────────┘
            │                    ▲
            ▼                    │
┌─────────────────────┐   ┌─────────────────────┐
│  FileWatcher         │   │  Indexer            │
│  （notify 实时监听）   │   │  （Rayon 并行）      │
│  → 事件防抖 300ms     │   │  提取 + 写索引       │
│  → 触发增量扫描       │   │  每 100 文件提交     │
└─────────────────────┘   └─────────────────────┘
```

### 关键设计决策

1. **相对路径存储**：数据库存**相对路径**（相对监控根），这样换电脑/换系统路径一致，可跨平台共享索引。
2. **MD5 内容哈希**：文件移位检测——内容相同但路径变了，通过 MD5 识别并更新路径，不必重新提取。
3. **定期自动提交**：每 100 文件 commit 一次，防止崩溃丢全部进度。
4. **文件移位识别**：不只是删+加，而是识别"这个文件搬家了"，保留原索引。

### 复现要点

```rust
// 增量扫描核心
for entry in walk(dir) {
    if excluded(entry) { continue }
    let rec = db.get_by_path(entry)?;
    if rec.mtime == entry.mtime && rec.indexed == 1 {
        continue // 没变，跳过
    }
    // 变了，重新提取索引
    indexer.batch_index(vec![entry])
}
```

### 边界与取舍

- 大目录扫描有 3 秒超时保护。
- 排除规则：`#/./~/tmp` 开头、`.git`/`__pycache__` 等精确名，用户可加自定义 glob。

---

## 五、模块四：AI 出口（LLM 网关调用）

### 解决什么问题

程序如何跟外部 LLM 通信。这是所有 AI 功能的共同底座。

### 核心设计：OpenAI 兼容 + 双模式

```
┌────────────────────────────────────────────┐
│  ai/mod.rs                                  │
│  ├─ chat(system, user) → Option<String>    │  // 非流式，一句话问完
│  ├─ chat_stream(system, user, on_delta)     │  // 流式，逐字回调
│  ├─ embed(text) → Option<Vec<f32>>          │  // 文本转向量
│  └─ resolve_active_endpoint(cfg, kind)      │  // 选当前启用的提供商
└────────────────────────────────────────────┘
```

### 为什么 OpenAI 兼容协议

绝大多数 LLM 服务（Ollama、OneAPI、vLLM、各种中转）都实现了 OpenAI 的 `/chat/completions` 接口。**只支持一种协议，就通吃所有模型**，不用为每个厂商写适配。

### 关键设计决策

1. **提供商抽象**：一个 `ProviderConfig` 记录 `base_url + api_key + 模型列表`。用户可在设置页添加多个，从中选一个做 embedding、一个做 LLM。
2. **`llm_enabled()` 优雅降级**：没配置 LLM 时，`chat`/`chat_stream` 直接返回 `None`，前端隐藏 AI 功能，普通搜索不受影响。
3. **流式优先**：`chat_stream` 逐字回调 `on_delta`，前端实时显示"打字机"效果；网关忽略 `stream:true` 时自动回退到非流式。
4. **取消机制**：全局原子布尔 `AI_CANCEL`，流式读取每行检查一次，可随时终止。
5. **熄屏降级**：LLM 是可选功能，配置错误或网关挂了，返回 `None` 而不是报错崩溃。

### 复现要点

```rust
// 流式核心
pub fn chat_stream(system, user, on_delta) -> ChatStreamOutcome {
    let req = ChatReq { stream: true, messages: [...], ... };
    let reader = http_post(base_url + "/chat/completions", &req.body)?;
    for line in reader.lines() {
        if ai_cancelled() { break }
        if let Some(delta) = parse_sse_delta(&line) {
            on_delta(delta, false);  // 逐字推给前端
            full.push_str(delta);
        }
    }
    ChatStreamOutcome { text: full, ... }
}
```

### 边界与取舍

- `content` 字段优先，`reasoning` 字段回退——有的模型把回答放 reasoning（如 deepseek-r1 类）。
- 连接超时 15 秒，但**流式读取无全局超时**（已知缺陷）。

---

## 六、模块五：语义向量检索（BGE 本地嵌入）

### 解决什么问题

关键词搜索查"字面"，但用户往往想查"意思"。比如搜"欠费催缴"，语义上应该也命中"逾期未缴纳物业管理费"。这就是语义搜索。

### 为什么用本地 BGE

- **隐私**：默认的 embedding 模型 `bge-large-zh-v1.5` 直接在本地用 ONNX 推理，文档内容不出机器。
- **中文优化**：BGE 是中文语义模型，效果优于通用英文模型。

### 核心架构

```
┌──────────────────────────────────────────────┐
│  ai/local_embed.rs                            │
│  LocalEmbedder（全局单例）                     │
│  ├─ init_local_embedder(dir, model)           │  // 加载 ONNX + tokenizer
│  ├─ embed_batch_local(texts) → Vec<Option>    │  // 批量编码
│  └─ embed_query_local(query) → Option<Vec>    │  // 单条查询编码
└──────────────────────────────────────────────┘
        │
        ▼
  tract-onnx（本地推理）+ tokenizers（分词）
```

### 向量的意义

文本 → 一串数字（如 1024 维向量）。语义相近的文本，向量距离近。用**余弦相似度**（cosine similarity）衡量：

```rust
fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    dot as f64  // 假设已归一化
}
```

### 语义检索怎么用（vector_full_scan）

```rust
// 对查询编码
let q_vec = embed(query)?;
// 遍历所有已存向量，算余弦，过滤阈值
let results = all_embeddings.iter()
    .filter(|(_, v)| cosine(&q_vec, v) >= 0.65)   // VECTOR_THRESHOLD
    .collect();
```

### BM25 + 语义融合

搜索结果**加权混合**：

```
最终分 = w × 语义分 + (1 - w) × BM25分
默认 w = 0.3（语义 30% / 关键词 70%）
```

用户在设置页可调滑杆。这保证：关键词命中仍占主导（BM25 可靠），语义提供"寻意外"的补充。

### 边界与取舍

- `vector_full_scan` 是全表扫描，无索引，1 万+ 条向量要 10-30 秒（已知缺陷）。
- 向量存 `doc_embeddings` 表（BLOB）。
- 只在配置了 embedding 且语义开关打开时才启用，否则纯 BM25。

---

## 七、模块六：AI 聊天与 RAG 管线

### 解决什么问题

`会话式文档问答`：用户既能直接问"判决结果是什么？"，也能指定范围（`@某文件`、`/ext:pdf`、目录等），程序检索相关文档后让 LLM 基于材料回答，还能多轮追问。

### RAG 是什么

RAG = **R**etrieval-**A**ugmented **G**eneration（检索增强生成）。核心思想：

> **不要问 LLM"记住的问题"，而是先把相关文档找出来塞给它，再让它基于这些材料回答。** 这样回答有据可依、不凭空捏造、可以标注来源。

### RAG 管线五步

```
┌─────────────┐  ┌──────────────┐  ┌─────────────┐
│ QueryRewrite │→│ ScopeResolver │→│ Retrieval   │
│ 查询改写      │  │ 范围解析      │  │ 三路检索     │
└─────────────┘  └──────────────┘  └──────┬──────┘
                                          ▼
┌─────────────┐  ┌─────────────┐  ┌──────────────┐
│  LLM 调用    │←│ 拼 context  │←│ Layer0/1/2   │
│  chat_stream │  │ 组装        │  │ 上下文注入    │
└─────────────┘  └─────────────┘  └──────────────┘
```

#### 第 1 步 QueryRewrite（查询改写）
- **为什么**：多轮对话里用户会说"它的风险呢？"这种省略句，必须补全成可检索的完整句。
- **规则改写**：合并上一问的关键词 + 当前问句，去停止词。
- **LLM 改写（可选）**：调用 LLM 补全指代；5 秒超时失败就回退规则。

#### 第 2 步 ScopeResolver（范围解析）
- **为什么**：用户可能限定在某个文件/目录/时间段内检索。
- 解析输入里的 `@文件`、`@目录`、`/ext:pdf`、`/date:...`、`/范围:...`，输出一系列过滤条件（`dir_ids`、`path_prefixes`、`file_ids`、`ext_filter`、`date`）。

#### 第 3 步 Retrieval（三路检索合并）
- ① BM25 关键词命中
- ② 语义向量命中
- ③ SQL 路径匹配（文件名含关键词）
- 三路合并去重，得到候选文件列表 `all_hits`。

#### 第 4 步 ContextAssembly（上下文组装）
- 把检索到的文件内容整理成 LLM 的 `system` 提示 + `user` 提问。
- **三层注入**（见下一节），控制哪些内容进 LLM、进多少。

#### 第 5 步 LLM 调用
- `chat_stream` 流式生成，逐字 emit `ai-chunk`，完成后 emit `ai-done`。

### 核心：上下文三层注入

这是 RAG 里**最关键也是最复杂**的部分——决定 LLM 能看到什么。

```
Layer 0: 引用文档（@某文件）
  → 用户明确指定的文件，全部注入，无条件
  → 用 chunked_or_truncated 按与查询相关的分块截断

Layer 1: BM25 命中补全
  → 自动检索到的、未被引用的文件
  → 默认最多 30 份全文，"全量召回"开启则全部
  → 每份按"总预算/份数"分配字符

Layer 2: 剩余命中摘要
  → 超出 Layer 1 的命中，批量生成摘要（每份简述）
  → 最多 60 份，并行 LLM 调用生成摘要
```

**设计理由**：LLM 上下文窗口有限，不能把几十万条命中全塞进去。分层设计保证：
- 用户**明确要求看的**（Layer 0）最可靠，全进。
- 自动检索的（Layer 1）按预算精选。
- 其余的（Layer 2）用摘要兜底，不遗漏大方向。

### "仅依据文档"严格模式（strict_docs）

一个硬开关。开启后：
- 引用文件必须在检索结果中出现（缺失/歧义/无内容报错）。
- 范围内无命中时**拒绝回答**（返回"未找到依据"），而不是让 LLM 瞎编。
- 旧对话的来源文件不混入本轮。

**为什么**：用户引用了文件，就说明答案必须基于它；如果检索没找到，老实承认比编造好。

### "全量召回"开关（full_recall）

控制 Layer 1 是否注入全部 BM25 命中。见下表——它只影响**非引用文档**的自动注入量。

### 内容截断的 6 种情况（为什么回答会遗漏）

| # | 阶段 | 舍弃什么 | 条件 | 如何避免 |
|---|------|---------|------|---------|
| 1 | Layer 0 | 引用文档超 50k 字符截断 | 引用文档很大 | 减少引用 |
| 2 | Layer 1 | 非引用只注入 30 份 | 全量召回关 | 开全量召回 |
| 3 | Layer 1 | 每份按配额截断 | 命中多配额少 | 缩小范围 |
| 4 | Layer 2 | 剩余只 60 份摘要 | 命中>90 | 缩小范围 |
| 5 | 总长度 | 拼起来截到 50k/140k | 始终存在 | 聚焦 |
| 6 | 语义 | 只取 500 条 | 全量召回关 | 开全量召回 |

### 复现要点

```rust
// 管线主流程（伪代码）
fn prepare_conversation_prompt(...) -> PreparedConversation {
    let search_q = rewrite_query(last_q, messages);      // 改写
    let scope = resolve_scope(&scope, dirs);             // 范围
    let hits = three_way_scan(search_q, scope);          // 检索
    let docs = layer0_mentions + layer1_bm25 + layer2_summaries; // 注入
    let context = truncate(docs.join("\n\n---\n\n"), 50000);
    PreparedConversation { system, user_msg, evidence, ... }
}
```

### 边界与取舍

- 内容截断**无位置感知**（从开头截），可能丢末尾结论——已知缺陷。
- 上下文窗口（150k）硬编码，不随模型调整。
- "全量召回"塞更多会挤占每文件篇幅，需要权衡。

---

## 八、模块七：会话与事件系统

### 解决什么问题

1. AI 聊天要有多轮对话，需要把会话（含历史、检索范围、每轮证据）持久化。
2. 流式生成要实时推给前端，需要一套事件通知机制。
3. 桌面（Tauri IPC）和浏览器（Web API）两种模式要共享前端。

### 会话存储

- 存 `chat_history.json`（一个 JSON 数组），每个会话含：标题、消息列表、来源文件、检索范围、每轮证据。
- 前端 `loading` 由 `pending_started_at` 驱动：发送时写入，`ai-done` 到达清除。

### 事件桥（Web 模式关键）

桌面端靠 Tauri 自带的事件总线，浏览器端没有——所以做一个**事件桥**：

```
Tauri emit("ai-chunk")
    │  listen_any 捕获
    ▼
event_tx.send(("ai-chunk", payload))
    │  broadcast channel
    ▼
/api/events (SSE)  →  前端 fetch 实时收到
```

`BRIDGED_EVENTS` 白名单列出要桥接的事件（`ai-chunk`、`ai-done`、`scan-progress` 等）。**注意 `ai-progress` 不在清单里**（Web 模式看不到进度条）。

### 前端 loading 状态机

```
发送 → pending_started_at=now → loading=true → 显示"思考中"+取消
ai-chunk → `setStreaming` → 流式文本
ai-done → loading=false → 追加回答 → 保存会话
取消 → cancel_ai_request() → 保留已输出 + "已取消"
```

### 复现要点

```rust
// 事件桥（webapi/mod.rs）
let (event_tx, _) = broadcast::channel(256);
for name in BRIDGED_EVENTS {
    app_handle.listen_any(name, move |event| {
        let _ = event_tx.send((name, event.payload()));
    });
}
```

### 边界与取舍

- 会话 JSON 无版本迁移（字段靠 serde default 兜底）——已知缺陷。
- 前端 `ai-done` 回调检查 `loadingRef.current`，时序问题会导致回复被丢弃——已知缺陷。

---

## 九、模块八：Web API 服务器（远程访问）

### 解决什么问题

桌面应用只能在本地用。加了 Web API 后，同一套前端代码也能在**浏览器**里跑（远程访问、移动设备访问），还能头一档尝鲜/演示。

### 设计思路

- **复用自己的代码**：后端命令逻辑完全复用（`conversation_ask_stream` 等），只是外层包一层 HTTP 路由 + Bearer token 认证。
- **同一套前端**：前端 `isTauri()` 检测——Tauri 环境走 IPC，浏览器走 HTTP，UI 代码零改动。
- **事件桥**：浏览器模式靠 `/api/events` SSE 接收 AI 流式事件（见模块七）。

### 安全设计

- **HTTPS + 自签名证书**（rcgen 生成），防明文。
- **Bearer token 认证**：所有 `/api/*` 都要 `Authorization: Bearer <token>`。
- 可配置端口、绑定地址。

### 复现要点

```rust
// 一个 AI 端点（webapi/routes/ai.rs）
async fn conversation_ask_stream_handler(State(state), Json(body)) -> Sse<...> {
    // 前台立即返回 SSE 流（触发后台任务）
    tauri::async_runtime::spawn(async move {
        commands::ai::conversation_ask_stream(...).await;
    });
    Ok(sse_for_session(rx, session_id))  // 监听事件桥
}
```

---

## 十、模块九：数据存储（SQLite）

### 设计哲学

一个数据库文件 `data.db` 存所有关系型数据。**几张表各司其职**：

| 表 | 存什么 | 为什么 |
|----|--------|--------|
| `file_tracking` | 每个文件的元数据（路径/mtime/size/md5/status/indexed） | 文件台账 |
| `content_index` | 每个文件的提取文本（按 md5 关联） | 内容缓存 |
| `doc_chunks` | 大文件分块 | 支持按块检索 |
| `doc_embeddings` | 每文件的语义向量 | 语义检索 |
| `doc_summaries` | 每文件的 LLM 摘要 | 摘要缓存 |
| `dir_config` | 监控目录配置 | 目录管理 |
| `search_history` | 搜索历史 | 历史记录 |
| `index_errors` | 索引失败记录 | 可诊断 |
| `app_settings` | 键值设置 | 通用设置 |
| `ai_events` | AI 推理事件链 | 推理追溯 |

### 关键设计决策

1. **`indexed` 字段用整型枚举**：0=未索引，1=已索引，2=提取中，3=已提取。配合部分索引 `WHERE indexed IN (0,2,3)` 快速找待处理文件。
2. **`md5` 是 key**：内容去重的核心——相同内容只存一份全文，多文件复用。
3. **`status` 软删除**：文件删除标记 `deleted` 而非物理删行，便于追溯与恢复。
4. **r2d2 连接池**：多线程访问 SQLite 需要池，避免死锁。

### 复现要点

```rust
// 文件台账核心 SQL
CREATE TABLE file_tracking (
    id      TEXT PRIMARY KEY,
    path    TEXT NOT NULL UNIQUE,   -- 相对路径
    md5     TEXT,                   -- 内容哈希（去重键）
    status  TEXT DEFAULT 'active',
    indexed INTEGER DEFAULT 0,      -- 0/1/2/3
    mtime   INTEGER,
    size    INTEGER
);
-- 部分索引：快速找待处理
CREATE INDEX idx_ft_pending ON file_tracking(updated_at)
    WHERE indexed IN (0, 2, 3);
```

---

## 十一、模块十：前端架构

### 设计思想

一套 React 代码，两种运行环境（桌面 / 浏览器），通过 `isTauri()` 检测自动切换 API 层。

### 页面映射

| 路由 | 页面 | 职责 |
|------|------|------|
| `/` | 搜索页 | 关键词/筛选/预览 |
| `/#/chat` | AI 聊天页 | 会话 + 对话 |
| `/#/browse` | 浏览页 | 表格浏览文件 |
| `/#/directories` | 资料库 | 目录管理 |
| `/#/index` | 索引状态 | 进度/统计 |
| `/#/settings` | 设置 | 配置 |
| `/#/logs` | 日志 | 查看日志 |

### ChatPanel（最重要的组件）

AI 聊天面板是前端最复杂的组件（755 行）：

```
ChatPanel
├── 消息列表（用户/助手消息 + Markdown 渲染）
├── 检索依据面板（AiEvidencePanel）
├── 推理时间线（AiEventTimeline）
├── loading 指示器 + 取消按钮
├── 范围条（chips）
├── 开关（仅依据文档 / 全量召回）
└── 输入 + 发送
```

### 双模式 API 客户端

```typescript
async function invoke<T>(cmd, args) {
  if (isTauri()) return tauriInvoke(cmd, args)      // 桌面: IPC
  const spec = MAPPINGS[cmd]                          // 浏览: HTTP
  const resp = await fetch(apiBase + spec.path, ...)
  if (spec.sse) { await resp.body.cancel(); return }  // SSE 触发
  return resp.json()
}
```

### 会话状态管理

- `AiChat` 持有 `activeSession` 状态。
- `ChatPanel` 通过 `onSessionChange` 回调 + `patchSession` 局部更新。
- 每次变更自动 `saveChatSession` 持久化。

---

## 十二、模块间数据流总览

```
                    ┌──────────────────────────────┐
                    │         前端 (React)           │
                    │  SearchPage / ChatPanel / ... │
                    └──────────┬───────────────────┘
                               │ isTauri() 自动切换
                 ┌─────────────┴──────────────┐
                 ▼                            ▼
        ┌───────────────┐            ┌─────────────────┐
        │ Tauri IPC     │            │ Web API (HTTP)  │
        └───────┬───────┘            └────────┬────────┘
                │                             │ 事件桥
                ▼                             ▼
        ┌──────────────────────────────────────────────┐
        │              Rust 后端 (commands)              │
        │  conversation_ask_stream / search / ...      │
        └──────────┬──────────────┬─────────────────────┘
                   │              │
        ┌──────────▼──────┐  ┌────▼────────────────────┐
        │  AI RAG 管线      │  │  搜索引擎 (Tantivy)      │
        │  改写→范围→检索→  │  │  + 语义向量 (BGE)       │
        │  注入→LLM        │  └───────────┬────────────┘
        └──────────┬──────┘              │
                   │                     │
        ┌──────────▼──────────┐  ┌───────▼─────────┐
        │  LLM 网关 (外部)      │  │  SQLite 数据库    │
        │  chat/completions    │  │  file/content... │
        └─────────────────────┘  └─────────────────┘
```

---

## 十三、核心设计原则

1. **性能优先**：全文搜索毫秒级，并行索引，增量更新。
2. **隐私可选**：核心全本地，AI 是可选且用户知情。
3. **优雅降级**：没配 LLM → AI 隐藏；LLM 挂了 → 返回 None 而非崩溃；语义没配 → 纯 BM25。
4. **够用不过度设计**：能用 SQLite 就不上 Postgres，能用标准库就不用框架。
5. **一套代码两处跑**：React 前端桌面/浏览器复用，后端命令 IPC/HTTP 复用。
6. **内容诚实**：RAG 回答必须基于材料，"仅依据文档"宁可拒绝也不编造。

---

## 十四、已知取舍与未来方向

### 当前取舍（可接受）

| 取舍 | 原因 | 改进空间 |
|------|------|---------|
| 全表向量扫描慢 | 简单、无需向量库 | 换 HNSW 索引 |
| 内容截断从开头 | 简单 | 分块重排（取最相关块） |
| 上下文窗口硬编码 | 简单 | 按模型动态调整 |
| Web 模式无进度条 | ai-progress 未桥接 | 补进 BRIDGED_EVENTS |
| 单进程 Web API | 简单 | 独立进程 |

### 未来方向

- 向量检索换 HNSW 索引（替代全表扫描）
- 分块 + 重排序（更精准的内容注入）
- 会话版本迁移机制
- 流式读取超时控制
- 多模型自动路由（RAG 不同步骤用不同模型）

---

*最后更新：2026-08-31*
