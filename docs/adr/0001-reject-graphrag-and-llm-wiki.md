# ADR-0001: 否决 GraphRAG 与 LLM Wiki 作为检索/知识架构

| 字段 | 值 |
|------|-----|
| 状态 | 已否决（Rejected） |
| 日期 | 2026-09-16 |
| 决策者 | 项目所有者 |
| 相关文档 | [`docs/research-rag-best-practices-personal.md`](../research-rag-best-practices-personal.md)（commit `c29d8c4`）、[`docs/rag-eval-baseline.md`](../rag-eval-baseline.md)（`scripts/eval/README.md` 指定的基线记录位置）、[`scripts/eval/README.md`](../../scripts/eval/README.md) |

---

## 背景（Context）

Link-Searcher 是一个本地优先的文档全文搜索工具，附带可选的 RAG 问答层。在 AI 功能逐步落地后，一个问题反复出现：是否应采用 Microsoft GraphRAG（arXiv [2404.16130](https://arxiv.org/abs/2404.16130), *From Local to Global*）或 Karpathy "LLM Wiki" 模式（gist [442a6bf](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f), 2026-04-04 发布）作为知识架构，以获得跨文档关系推理和全局主题综合能力。

项目自身的调研报告 `docs/research-rag-best-practices-personal.md`（2026-09-02, commit `c29d8c4`）已经独立考察了这两种方案，并基于实测语料规模得出了明确结论。该报告是本 ADR 的权威先验材料。本 ADR 的作用是将该报告的分析结论正式确立为架构决策，使该问题无需在每次讨论中重新论证。

当前架构（参见 `docs/ARCHITECTURE.md` 模块四至六：AI 出口 / 语义向量检索 / AI 聊天与 RAG 管线）：

- 混合检索：BM25（Tantivy）+ 文件级向量 + chunk 级向量，加权融合
- 两级漏斗：文档级粗筛（BM25 + 文件级向量）→ 对命中 md5 集做 chunk 级余弦精检（commit `1b9908e`）
- 有范围限定的 RAG 对话：查询改写、引用编号校验、范围控制（@文件/@目录/strict 模式）、`ai_events` 推理时间线

---

## 决策（Decision）

**不采用 GraphRAG，也不采用 LLM Wiki 模式。**

现有架构（混合检索 + 两级漏斗 + 有范围限定的 RAG 对话）保持不变。`ai_topic_clusters` 命令（`commands/ai.rs:108`）继续作为全库主题概览的轻量替代。

---

## 理由（Rationale）

### 理由 1：规模错配（Scale Mismatch）

LLM Wiki 模式和 GraphRAG 各自有明确的有效规模区间。Link-Searcher 的实测语料远超前者上限，也超出后者的合理落地范围。

**LLM Wiki 模式的规模上限：**

`docs/research-rag-best-practices-personal.md:20` 原文：

> 对几百篇以内的个人文档，"编译为结构化 Markdown wiki（索引 + 概念页 + 双链）"往往优于向量 RAG；但它有代价：入库要用较强模型做编译、需要持续维护、**超过约 150–200 页/文件后超出单模型上下文**。

Karpathy gist 原文（主 Agent 2026-09-16 核实 URL 有效）：该模式 "works surprisingly well at moderate scale (~100 sources, ~hundreds of pages)"。三层结构（`raw/` 人写不可变源、`wiki/` LLM 写 Markdown、schema 文件如 `CLAUDE.md`/`AGENTS.md` 双方共维护），三操作（Ingest / Query / Lint），`index.md` 作目录、`log.md` 作日志。

**GraphRAG 的定性：**

`docs/research-rag-best-practices-personal.md:548` 原文：

> GraphRAG 出处（用于说明"全局综述问题"与向量 RAG 互补，**而非小语料必选**）。

**实测语料规模（SQL 度量，2026-09-16，`/Volumes/Data/index/data.db`，1.1 GB，`sqlite3 -readonly`）：**

| 表 | 行数 | 说明 |
|---|---:|---|
| `file_tracking`（status='active'） | 11,825 | 活跃文件 |
| `content_index` | 9,355 | < active 文件属预期，按内容 MD5 去重，重复内容共享一行 |
| `doc_chunks` | 125,243 | 文档分块 |
| `doc_embeddings` | 11,682 | 文件级向量 |
| `chunk_embeddings` | 125,243 | chunk 级向量 |
| `doc_summaries` | 0 | 摘要缓存未使用（`ai_topic_clusters` 回退到 `content_index` 前 200 字） |

`doc_chunks : chunk_embeddings` = **1.0**。2026-09-02 该比例为 ~100:1（`docs/research-rag-best-practices-personal.md:359` 记录 doc_chunks 12.5 万行、chunk_embeddings 仅 1154），commit `1b9908e` 的增量回填（`missing_chunk_embedding_md5s` 改为 LEFT JOIN 选部分缺块的 md5、按缺块数升序、`existing_chunk_indexes` 跳过已有）将其收敛到 1:1。

`docs/research-rag-best-practices-personal.md:359` 记录（实测 2026-09-02）最大库 `com.link-searcher.app`：

> 22,441 个 active 文件、2.65 亿字，其中 75% ≤1 万字、82 篇 >20 万字、5 篇 >100 万字（最长 667 万字）

**结论：** LLM Wiki 模式的有效区间是 ~100 个来源、几百页（调研报告给出的上限为约 150–200 文件）。实测两个库相对该上限的倍数：

| 库 | 活跃文件 | ÷200（上限高端） | ÷100（上限低端） |
|---|---:|---:|---:|
| `/Volumes/Data/index`（2026-09-16 实测） | 11,825 | 59× | 118× |
| `com.link-searcher.app`（2026-09-02 实测） | 22,441 | 112× | 224× |

即两个库均超出该模式有效区间 **59 至 224 倍**，且各自都含 12.5 万级 chunk。GraphRAG 论文本身将其定位为"全局综述"与向量 RAG 互补，而非小语料的必选项。项目调研报告已独立得出此结论。

### 理由 2：成本不可行（Cost Infeasibility）

GraphRAG 索引对每个文本单元调用一次 LLM 提取实体/关系/声明，再对社区做摘要。在实测语料上，这意味着社区摘要之前就有 ≥125,000 次 chunk 级 LLM 调用。关键约束：GraphRAG 无原生增量更新，设计为一次性离线索引；而 Link-Searcher 的语料被 `notify` 文件监控器（300 ms 防抖，`docs/ARCHITECTURE.md:160`）持续监听，文件增删改随时发生。任何语料变更都意味着重跑大量管线步骤。

LLM Wiki 模式同样需要用强模型做全量编译，且需要持续维护（`docs/research-rag-best-practices-personal.md:20`）。

**项目已明确的不做清单（`docs/research-rag-best-practices-personal.md:503` 原文）：**

> 不做全库 chunk ANN/HNSW（两级漏斗替代——HNSW 不支持删除，桌面增删改是硬伤，且全库 chunk 向量 2GB 级不配常驻）；不做全库 chunk 暴力扫描（新规模下不再免费，P1-A 替代）；不做 LLM rerank（亏本）；不做 multi-query/HyDE（叠满更差）；不换 bge-m3（实际生效已是 bge-large-zh-v1.5，重嵌 2.2 万 doc + 12.5 万块不值）；不做整库进上下文（单文档都可能超 20 万 token）

**证据表：**

| 证据 | 位置 | 与本决策的关联 |
|------|------|--------------|
| 明确不做清单 | `docs/research-rag-best-practices-personal.md:503` | 项目已独立否决全库 LLM 重算、全库 ANN、全库进上下文等 GraphRAG/LLM Wiki 的前置依赖 |
| 文件监控 + 增量更新 | `docs/ARCHITECTURE.md:160`; AGENTS.md scanner 模块 | 语料持续变动，一次性离线索引（GraphRAG 设计前提）与实际使用模式冲突 |
| 两级漏斗已替代全库扫描 | commit `1b9908e`; `commands/ai/prompt.rs`（漏斗编排，约 L534-561）; `ai/mod.rs:532`（`chunk_vector_scan_for_md5s`）; `db/tracker/embeddings.rs:171`（`get_chunk_embeddings_by_md5s`） | chunk 级余弦只对粗筛命中集做，已将扫描量从全库 chunk 降到命中集 chunk，无需全库索引 |
| chunk 向量增量回填 | 实现：commit `1b9908e`；问题：`docs/research-rag-best-practices-personal.md:455`（1154 / 125,243 ≈ 1%）；结果：本文理由 1 规模快照（2026-09-16 实测比值 1.0） | chunk 向量覆盖率从 ~1% 收敛到 100%，证明增量路径可行；GraphRAG 无此路径 |

### 理由 3：已有能力覆盖 + 实测无缺口（Existing Coverage + No Measured Gap）

项目已经以低成本方式实现了这两种技术所提供的核心能力：

| 能力 | 已有实现 | 代码位置 |
|------|---------|---------|
| 全库主题概览 | `ai_topic_clusters` 命令：拉取文档摘要/前 200 字，单次 LLM 调用做主题分组 | `commands/ai.rs:108`; `db/mod.rs:323`（`doc_summaries` 表）; `lib.rs:101`（注册） |
| 跨文档综合 + 引用 | RAG 对话管线：查询改写、四路检索、引用编号校验、范围控制 | `commands/ai.rs:563`（`conversation_ask_stream`） |
| 规模适配检索 | 两级漏斗：文档级粗筛 → 命中集 chunk 级精检，粗筛 0 命中时回退全库 chunk 扫描 | commit `1b9908e`; `commands/ai/prompt.rs`（漏斗编排，约 L534-561）; `ai/mod.rs:532`（`chunk_vector_scan_for_md5s`）; `db/tracker/embeddings.rs:171`（`get_chunk_embeddings_by_md5s`） |
| 检索质量度量 | 评测脚本：golden set + `chat --dry-run`，输出 Context Recall@10 / Success@10 | commit `d99ce91`; `scripts/eval/run_rag_eval.sh`; `scripts/eval/README.md` |
| 生产链路收敛 | 删除 `ai/skills/` 脚手架，收敛到单一生产路径 | commit `a49265c`（2026-09-03） |

**评测现状：**

冒烟跑（3 个问句）报告 Recall@10 = 100%（`CHANGELOG.md:1859`：smoke 验证）。reranker 因此被延后（`CHANGELOG.md:1867`：P2-4 reranker 评测未证明需要，当前 Recall@10=100%）。

必须诚实说明：3 个问句的样本过小，不能作为"检索质量已无瓶颈"的结论性证据。评测基础设施已经存在，但其当前样本不足以产生可信结论。`scripts/eval/README.md:52` 指出 golden set 应标注 30 至 60 条问句。在 golden set 扩充到该规模并跑出可信基线之前，"无缺口"这一判断的置信度有限。扩充 golden set 是后续工作的前置条件，不是本 ADR 的内容。

### 前提澄清

**本决策的否决理由不是"项目缺少 LLM 基础设施"。** 项目已包含完整的 LLM 子系统：

| 组件 | 位置 | 功能 |
|------|------|------|
| OpenAI 兼容网关 | `src-tauri/src/ai/mod.rs` | `chat` / `chat_stream` / `embed_batch`（`ureq`） |
| 多提供商配置 | `src-tauri/src/config.rs` | `ProviderConfig`（`base_url` + `api_key`），CRUD，配置写 0600 |
| 本地 embedding | `src-tauri/src/ai/local_embed.rs` | 本地 BGE（`tract-onnx`），完全离线 |
| RAG / 对话 / 聚类 | `src-tauri/src/commands/ai.rs` | 查询改写、检索、注入、引用校验、主题聚类 |
| 前端配置 UI | `src/components/settings/AiTab.tsx` | 提供商配置界面 |

否决基于成本、规模、冗余三个维度的考量，不是基于基础设施缺失。采用 GraphRAG 或 LLM Wiki 不需要引入新的子系统，但会在现有子系统上叠加不可接受的全库 LLM 调用成本和一次性离线索引的维护负担。

---

## 触发重审的条件（Re-review Triggers）

以下条件任一满足时，本决策可被重新评估。每条都是工程师可实际度量的具体条件。

1. **Golden set 扩充到 30 至 60 条问句后，Context Recall@10 跌破 90%。**
   - 度量方式：按 `scripts/eval/README.md` 准备 golden 目录，跑 `scripts/eval/run_rag_eval.sh`，读取 Recall@10 输出。
   - 阈值理由：90% 意味着每 10 个相关文档有 1 个被漏检。当前冒烟样本（3 问句, 100%）太小，阈值仅在 golden set 达到推荐规模（30 至 60 条, `scripts/eval/README.md:52`）后才有统计意义。低于 90% 时，引入实体/图结构检索层的成本可能被可度量的检索失败所证明。

2. **出现一个具体的、真实的用户需求：跨文档多跳关系查询或全库主题综合，且当前 RAG 可证明无法满足。**
   - 度量方式：将该用例加入 golden set，跑评测确认 Recall@10 = 0（或回答正确性被 judge 判定为失败），而非假设性推理。
   - `ai_topic_clusters` 命令已提供主题分组（`commands/ai.rs:108`）。触发条件是该命令或现有 RAG 对话在某个具体问题上可证明失败，且该问题类型是用户实际需要的，不是想象出来的。

3. **语料规模在任一方向发生实质性变化：**
   - (a) 活跃文件降至 ~200 以下：LLM Wiki 模式进入其验证有效区间（~100 sources, per Karpathy gist），该选项重新开放。
   - (b) doc 级向量增长到数十万：重开 ANN / 内存驻留问题。项目已在 ~11k 向量时暂缓内存驻留（`CHANGELOG.md:1865`：doc 向量 1.1 万条全表拉 + 余弦约几十 ms 非瓶颈，推迟到 doc 向量几十万级再做）。
   - 度量方式：`SELECT COUNT(*) FROM file_tracking WHERE status='active'` 和 `SELECT COUNT(*) FROM doc_embeddings`。

4. **出现产品（非检索质量）层面对知识图谱可视化的需求。**
   - 这是一个产品决策，不是检索架构决策。在考虑 GraphRAG 之前，首先应评估是否能低成本可视化现有 `ai_topic_clusters` 输出（`commands/ai.rs:108`），该命令已按主题分组文档。
   - 度量方式：有产品规格或用户请求明确要求图结构可视化。

---

## 考虑过的替代方案（Alternatives Considered）

### (i) 对用户选定子集（50 至 100 篇）做 scoped LLM Wiki，而非全库

**未采纳。** 现有 scoped RAG 对话（`conversation_ask_stream` + @文件/@目录范围限定）已经覆盖"对这几篇文档提问"的用例，无需额外编译 wiki。在 50 至 100 篇上编译 wiki 会重复已有的范围控制能力，同时引入 LLM 编译成本和持续维护负担。LLM Wiki 的价值在于全库结构化概览，而非子集问答；子集问答是现有 RAG 的本职。

### (ii) 用现有 LLM 调用构建轻量 entity→file 倒排索引，替代完整 GraphRAG

**未采纳，延后。** `ai_topic_clusters` 已通过单次 LLM 调用提供主题级分组。实体倒排索引需要新增表、提取提示词、增量更新逻辑，用于服务关系查询——而 golden set 中没有此类用例，也没有用户报告此类需求。如果触发条件 #2 满足（出现具体的关系查询失败用例），可重新评估。

### (iii) 用本地模型跑 GraphRAG，而非云 API

**未采纳。** 项目的 LLM 子系统支持本地和 API 两种模型。但 GraphRAG 索引在实测语料上需要 ≥125,000 次 chunk 级 LLM 调用。按项目实测的本地推理吞吐（embedding: bge-small-zh-v1.5 ONNX/CPU 48 至 100 块/s, `CHANGELOG.md:1873`; LLM 生成: 14B Q4 约 10 至 40 tok/s, `docs/research-rag-best-practices-personal.md:180`），实体提取阶段是数小时的离线任务，且无增量更新路径。语料被 `notify` 监控器（300 ms 防抖）持续变动，任何变更都需要重跑管线的大部分步骤。本地模型不解决成本和增量更新的根本问题，只把成本从 API 费用变为本地计算时间。

---

## 后果（Consequences）

### 代价

- **无图结构关系查询能力。** 多跳关系查询（如"同时提及实体 A 和实体 B 的文档"）不是原生支持的。BM25 + 向量混合检索可能漏掉跨文档关系模式，知识图谱能捕获但本架构不提供。
- **全库主题综合是粗粒度的。** `ai_topic_clusters` 通过单次 LLM 调用对文档摘要/前 200 字做分组，不是 GraphRAG 的社区级层次化摘要。对极大量语料，这是更粗的近似。

### 保护

- **本地优先隐私。** 不会有全库 LLM 索引将所有内容发送到 API。现有 RAG 只将查询检索到的 chunk 发送到配置的网关。
- **无全库重建。** `notify` 监控器 + 增量 chunk 回填（commit `1b9908e`）处理语料变更，无需重跑全局索引管线。
- **无新的大成本面。** 无 per-chunk LLM 提取调用、无社区摘要、无图数据库或图维护工具。

---

## 未核实内容声明

本 ADR 的所有事实性主张均来自 `docs/research-rag-best-practices-personal.md`（该报告的来源核实状态见其附表，`docs/research-rag-best-practices-personal.md:527-555`）或仓库内可验证的代码与 commit。

以下内容曾由一个研究子代理报告，但**未经核实**，在本 ADR 中**未被用作证据**：

- 一篇据称标题为 "Self-Evolving Agent-Native Retrieval via LLM-Wiki" 的 arXiv 论文（报告 ID `2605.25480`），声称相较 GraphRAG 有 2.0 至 8.1 的 F1 提升。该论文的标题、ID 与结论**均未经独立核实**（未定位、未阅读），因此在本 ADR 中不作为证据。
- 第三方 LLM Wiki 实现的精确 GitHub star 数。
- 一个 `microsoft/llmwiki` VS Code 扩展。

这些内容在此仅作记录，表明它们曾被考虑并作为证据排除。除非独立核实，不应在后续讨论本决策时引用。
