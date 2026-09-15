# RAG 检索层评测基线

`scripts/eval/run_rag_eval.sh`（commit `d99ce91` 引入）是**全项目唯一的检索质量度量**：
可运行，但只有当基线数字被固定下来时，门禁才可执行。本文件就是那个基线数字的
唯一登记处。每次跑完评测，把结果追加到文末的[变更记录](#变更记录)表；任何涉及
检索/注入的改动，对照本文件判断是否回归。

## 目的

- 让 RAG 检索门禁从"可运行"变成"**可执行**"：评测脚本输出数字后，必须有地方记录
  被固定下来的基线，改动前后才能对比。
- 是本项目检索基线数字的**唯一权威来源**，避免数字散落在 commit message、日志
  或聊天记录里无从查证。

## 评测方法与指标

方法与 `scripts/eval/run_rag_eval.sh` 完全一致：对 golden 集每个问句跑
`link-searcher chat --dry-run`（三路检索 + 注入，**不调用 LLM**），解析输出中
`path=(\S+)` 的注入 evidence（按相关度排序），取前 10 个 basename，与该问句标注的
`support_files` 按 basename 比对。

| 指标 | 定义 | 抓住什么 |
|--------|------|---------|
| **Context Recall@10** | golden 标注的支撑文件里，有多少比例出现在注入 evidence 前 10 条 | 该有的材料没检索到 |
| **Success@10** | 多少问句至少有 1 个支撑文件进入前 10 条 | 完全没检索到 |

生成层不在本评测范围：回答是否忠实、引用是否准确，需要时用真实 `chat` 回答 +
较强 API 模型做 judge 单独评。

## 前置条件

1. **可执行文件**：`src-tauri/target/debug/link-searcher`（`cd src-tauri && cargo
   build`，debug 即可）。可用环境变量 `LS_BIN` 覆盖路径。
2. **golden 目录**：含 `docs/`（评测语料，会被索引）与 `golden.jsonl`（每行
   `{"question": "...", "support_files": ["a.txt", ...]}`）。`support_files` 用
   basename 匹配。
3. **embedding 配置**：`active_embedding_model_id` 必须已配置（设置页或
   `config`）。未配置本地 BGE（`local:bge-large-zh-v1.5`）或远端网关时，向量通道
   **静默跳过**，结果只反映 BM25 + 路径通道，不能代表真实检索质量。
4. **隔离数据目录**：`LINK_SEARCHER_DATA_DIR` 指向独立 data dir，不污染真实索引库。
   省略 `data_dir` 参数时脚本自动用 `mktemp` 临时目录（此时 `docs/` 会被自动
   `scan` 索引）。

## 真实库规模快照

> 快照具有时效性：数字标注测量日期，仅供理解"评测对象有多大"，不是常量。

**主库**（作者 2026-09-16 用 `sqlite3 -readonly` 测量
`/Volumes/Data/index/data.db`，1.1 GB，最后写入 2026-09-15 17:30）：

| 表 | 行数 |
|------|------|
| `file_tracking`（status='active'） | 11,825 |
| `file_tracking`（全部） | 12,052 |
| `content_index` | 9,355 |
| `doc_chunks` | 125,243 |
| `doc_embeddings` | 11,682 |
| `chunk_embeddings` | 125,243 |
| `doc_summaries` | 0 |

复测 SQL：

```sql
SELECT 'file_tracking(active)' AS metric, COUNT(*) AS n FROM file_tracking WHERE status='active'
UNION ALL SELECT 'content_index', COUNT(*) FROM content_index
UNION ALL SELECT 'doc_chunks', COUNT(*) FROM doc_chunks
UNION ALL SELECT 'doc_embeddings', COUNT(*) FROM doc_embeddings
UNION ALL SELECT 'chunk_embeddings', COUNT(*) FROM chunk_embeddings
UNION ALL SELECT 'doc_summaries', COUNT(*) FROM doc_summaries;
```

解读（均为有意为之，不是缺陷）：

- `content_index`（9,355）< active 文件（11,825）：**按设计如此**。`content_index`
  以内容 MD5 为键，相同内容的文件共享一行（内容去重），因此行数必然小于等于文件数。
- `doc_chunks : chunk_embeddings` = **1.0**（2026-09-16）：chunk 与向量一一对应。
  此前 2026-09-02 该比值约为 100×（见 `docs/research-rag-best-practices-personal.md`），
  由 commit `1b9908e` 的两级检索漏斗 + 增量 chunk 回填修复。
- `doc_summaries` = 0：摘要缓存在本库无行。该功能在代码中存在，只是本语料尚未
  触发（生成摘要需要用户手动点击「✦ AI 摘要」，且按文件缓存）。
- `doc_embeddings` 全量驻内存扫描约"几十 ms"（~11k doc 向量），当前不是瓶颈；
  到几十万规模再重新评估（已在 CHANGELOG 记录延迟处理）。

**第二库**（作者 2026-09-02 测量，见 `docs/research-rag-best-practices-personal.md`）：
`com.link-searcher.app` —— 22,441 个 active 文件，约 2.65 亿字，75% 的文档
≤1 万字，82 个文档 >20 万字，5 个 >100 万字（最长 667 万字）。

**为什么规模值得关心门禁**：主库有 12.5 万 chunk，第二库是 2.65 亿字的超长文档
分布。检索质量直接决定 RAG 回答的材料基础；recall 在如此规模上掉几个百分点，
意味着每轮问答稳定漏掉若干支撑材料。没有基线数字，这类回归无法被任何人察觉。

## 当前基线状态

**严格的诚实状态：本仓库没有任何已提交的 golden 集。** 真实案件材料含当事人信息，
按 `scripts/eval/README.md` 的设计**不提交 git**（评测脚本本身入库，语料目录由
`.gitignore` 覆盖，见[局限](#局限)）。

**至今唯一记录过的数字**：`d99ce91`（2026-09-03）的 commit message 记录了一次
**3 问句语料**的冒烟运行，`Recall@10 = 100%`。

⚠️ 这个样本量对门禁而言**远远不够**：3 问句什么都证明不了（任何一次改动都可能
刚好把这 3 条问句的检索路径弄坏又弄好）。当时以此 100% 作为"不需要 reranker"
（P2-4）的论据之一，该结论应在大样本下重新验证。按 `scripts/eval/README.md`，
有意义的基线需要 **30–60 条人工标注问句**（事实抽取、跨文件综合、精确编号匹配、
不可答各若干）。

门禁就绪度：

| 项 | 状态 |
|------|------|
| 评测脚本（`run_rag_eval.sh`） | ✅ 已入库（`d99ce91`） |
| 基线登记文件（本文件） | ✅ 2026-09-16 创建 |
| golden 语料集（30–60 问句） | ❌ 未制作（隐私设计，勿入库） |
| 正式基线数字（≥30 问句） | ❌ 未建立（仅有 3 问句冒烟 100%） |

当前门禁是"**必须测量**"型：任何涉及检索/注入的改动合入前必须跑评测并在此登记
一个新数字作为参考；等 golden 集达到 30–60 问句规模后，升级为"**对比基线**"型。

## 如何运行

```bash
# 1. 构建 CLI（debug 即可）
cd src-tauri && cargo build

# 2. 准备 golden 目录
#    golden_dir/
#      docs/            # 评测语料（被索引的文档）
#      golden.jsonl     # 每行 {"question": "...", "support_files": ["a.txt", ...]}
# 3. 跑评测（省略 [data_dir] 时用临时目录并自动索引 docs/）
bash scripts/eval/run_rag_eval.sh <golden_dir> [data_dir]
```

可选环境变量：

- `LS_BIN`：可执行文件路径（默认 `src-tauri/target/debug/link-searcher`）
- `LINK_SEARCHER_DATA_DIR`：传入 `data_dir` 时由脚本导出，指向隔离 data dir

输出格式示意（⚠️ 以下数字为格式演示，**不是本仓库的真实结果**；真实结果仅在
[变更记录](#变更记录)表中登记）：

```
  RAG 检索评测结果（N 问句）
  Context Recall@10: xx.xx% (n/m)
  Success@10 (≥1 支撑文件进 top-10): xx.xx% (a/b)
```

跑完把结果追加到下方[变更记录](#变更记录)表，一行一条。

## 门禁规则

以下任一改动合入前，**必须**跑本评测并对比 / 登记基线：

- 检索逻辑（两级漏斗、RRF 融合、查询改写、范围解析）
- 注入逻辑（evidence 组装、编号体系、`[N]` 引用编号）
- chunk 切分（大小、重叠、切分策略）
- 注入预算（budget 调整）
- embedding（模型更换、向量通道开关、LRU 缓存行为）

**数字回落即回归**：对照本文件最后登记的真实基线（不是冒烟数字），Recall@10
或 Success@10 下降即视为回归，改动不予合入，除非下降有明确收益（如成本、延迟）
并在变更记录中注明权衡。

## 局限

- **dry-run 不调用 LLM**：输出的是注入的 evidence 而非回答，因此无法覆盖生成层
  （忠实性、引用准确性）的退化。
- **依赖 embedding 配置**：未配置 `active_embedding_model_id` 时向量通道静默跳过，
  结果只反映 BM25 + 路径通道，且不会报错，容易误判为全通道结果。
- **golden 语料不在 git 里**：评测无法被他人直接复现。`.gitignore` 的
  `scripts/*` 规则（含注释 "eval corpora, private fixtures"，仅放行几个
  setup/clean 脚本）已覆盖 `scripts/eval/` 下所有新文件，因此
  `scripts/eval/golden/` **不需要单独加条目**，放进该路径即自动被忽略；
  `scripts/eval/` 下的评测脚本是显式入库的例外。
- **生成层不在范围**：回答是否忠实、引用是否准确，需用真实 `chat` + 较强 API
  模型做 judge 单独评。
- **小样本**：当前只有 3 问句冒烟记录，任何以此为基线的对比都不可信，目标是
  30–60 条人工标注问句。

## 变更记录

| 日期 | 问句数 | Recall@10 | Success@10 | 说明 |
|------|:---:|:---:|:---:|------|
| 2026-09-16 | — | — | — | 本文件创建：门禁基线登记入口。尚无正式 golden 集与基线（仅 3 问句冒烟 100%，见上文） |

以后每次评测在此表**追加**一行（日期、问句数、两个指标、语料/环境说明），
保持"最新一行 = 当前基线"。