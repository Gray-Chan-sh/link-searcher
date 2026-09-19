# RAG 检索层评测（Context Recall@10）

`scripts/eval/run_rag_eval.sh` 提供可重复的检索质量基线：对 golden set 的每个
问题跑 `link-searcher chat --dry-run`（三路检索 + 注入，**不调用 LLM**），把注入
的 evidence（按相关度排序）与该问句标注的支撑文件比对，输出 Context Recall@10
与 Success@10。

**作用**：任何改动（检索逻辑、分块、注入预算、embedding、编号体系）前后跑同一
评测，用数字证明"更准了 / 没变差"——这是全项目唯一的检索质量度量。

## 用法

```bash
# 1. 构建 CLI（debug 即可）
cd src-tauri && cargo build

# 2. 准备 golden 目录（见下），然后跑评测
bash scripts/eval/run_rag_eval.sh <golden_dir>
```

golden 目录结构：

```
golden_dir/
  docs/            # 评测语料（被索引的文档）
  golden.jsonl     # 标注：每行 {"question": "...", "support_files": ["a.txt", ...], "category": "fact"}
```

`support_files` 用 **basename** 匹配（不依赖绝对路径），支撑文件应能唯一对应
`docs/` 下的某个文件。

### `category` 分类（可选但强烈建议）

给每题打**恰好一个**主类别，评测会按类别分桶输出 Recall/Success —— 这样任何改动
都能回答"**哪一类变好、哪一类变坏**"，而不是只有一个总数。缺省记为 `uncategorized`。

| category | 含义 |
|---|---|
| `fact` | 单文档事实，措辞含关键词（BM25 友好） |
| `semantic` | 单文档事实，但措辞语义化（问句用词与文件名不重叠，压向量通道） |
| `exact_id` | 精确编号/名称匹配（案号、信用代码、精确金额） |
| `twin` | 近名/同名文件区分（须靠内容区分兄弟文件） |
| `multi_hop` | 跨文件综合（2–3 份支撑） |
| `long_doc` | 长文档深部事实（>1 万字，压 chunk 通道） |

### 真实库子集（推荐）

真实分布（超长文档、真实案件材料）比合成语料更能暴露检索缺陷。做法：

1. 从真实监控目录复制一批**有代表性**的文档到 `golden_dir/docs/`（含长文档、
   中文案件材料、表格等）。
2. 人工标注 30–60 条问句：每条问题应能从 `docs/` 中**唯一确定** 1–3 个支撑文件
   （事实抽取、跨文件综合、精确编号匹配、不可答各若干）。
3. 跑评测，把结果记录到 `docs/rag-eval-baseline.md`。

⚠️ **隐私**：真实案件材料含当事人信息，**不要提交 git**。评测脚本本身入库，
语料目录加进 `.gitignore`（如 `scripts/eval/golden/`）。

### 注意

- dry-run 走 `prepare_conversation_prompt`，**依赖 embedding 配置**。未配置本地
  BGE（`local:bge-large-zh-v1.5`）或远端网关时，向量通道静默跳过，结果只反映
  BM25 + 路径通道。评测前确认设置页/`config` 的 `active_embedding_model_id`。
- 评测用独立 data dir（`LINK_SEARCHER_DATA_DIR`），不污染真实索引库。
- 🚨 **不要并行跑多个评测**。本评测逐题启动 CLI 进程，每题都要做一次本地 BGE 嵌入；
  并行运行会争抢 CPU，使嵌入超过代码里的 **5 秒超时**（`semantic_fuse` 的
  `cached_embed`），从而**静默退化为纯 BM25**，指标显著偏低。实测同一 75 题集：
  并行 `Success@10 56.00%` vs 串行 `62.67%`；语义题子集 并行 `3/20` vs 串行 `7/20`。
  **做 A/B 对比时务必串行执行**（一次只跑一组）。
- **语义权重 A/B**：环境变量 `LINK_SEARCHER_SEMANTIC_WEIGHT`（0.0–1.0）可覆盖
  `semantic_weight`（默认 0.3）而**不改动用户 `config.json`**，用于对比不同权重下的
  检索质量。例：
  ```bash
  LINK_SEARCHER_SEMANTIC_WEIGHT=0.7 bash scripts/eval/run_rag_eval.sh <golden_dir> <data_dir>
  ```
  ⚠️ 做权重对比时，golden 集需包含**真·语义题**（问句用词与答案文件名不重叠）——
  若全是"文件名即答案"的关键词题，结论必然偏向 BM25。

### 其他评测用覆盖开关（免重建做参数扫描）

| 环境变量 | 默认 | 作用 |
|---|---|---|
| `LINK_SEARCHER_SEMANTIC_WEIGHT` | 0.3 | 语义权重（RRF 下只影响 BM25 候选内部重排） |
| `LINK_SEARCHER_FUSION` | （空=RRF） | 设为 `mix` 退回旧的分数加权融合 |
| `LINK_SEARCHER_VECTOR_THRESHOLD` | 0.55 | 文件级向量相似度阈值 |
| `LINK_SEARCHER_CHUNK_VECTOR_THRESHOLD` | 0.55 | chunk 级向量阈值 |
| `LINK_SEARCHER_CHUNK_TOP_K` | 500 | chunk 通道取数上限 |
| `LINK_SEARCHER_RRF_CHUNK_WEIGHT` | 1.0 | chunk 通道的 RRF 权重 |
| `LINK_SEARCHER_RERANK` | （空=开） | 设为 `off` 关闭重排阶段 |
| `LINK_SEARCHER_RERANK_TOP_N` | 50 | 重排候选窗（⚠️ 开大显著变慢，1000 会超时） |
| `LINK_SEARCHER_RERANK_FUSION` | 0.3 | 重排名次与原次序的 RRF 融合权重 |
| `LINK_SEARCHER_BM25_ELITE_K` | 0（关） | 给**纯 BM25 前 K 名**加权重 |
| `LINK_SEARCHER_BM25_ELITE_WEIGHT` | 4.0 | 上述加权倍数 |
| `LINK_SEARCHER_INNER_SEMANTIC_FUSE` | （空=开） | 设为 `off` 使 BM25 通道保持纯 BM25 名次 |
| `LINK_SEARCHER_RRF_BM25_WEIGHT` | 1.0 | BM25 通道整体权重（⚠️ 全局调高会灌满前 30） |

⚠️ **`BM25_ELITE_*` 是"已知代价的应急开关"**：`K=10 w=2` 可让典型 BM25 强/语义中等的
文档进 top-10，但实测总分 **−8.00pp**（`multi_hop` 75→37.5）。见
`docs/rag-eval-baseline.md` 的「指标盲区案例」节。**默认关闭，勿轻易采纳。**

⚠️ **已扫描结论（2026-09-17，75 题串行）**：阈值/通道权重这类参数**均已到极限** —— 调整只会在
`semantic`/`long_doc` 与 `multi_hop`/`twin` 之间移动（净变化恒 +1 题、p=0.50）。**基线
（0.55 / 0.55 / 500 / 1.0）即为当前最优**，无需再调。

## 指标含义

- **Context Recall@10**：golden 标注的支撑文件里，有多少比例出现在注入 evidence
  前 10 条。抓"该有的材料没检索到"。
- **Success@10**：多少问句至少有 1 个支撑文件进前 10。抓"完全没检索到"。

生成层（回答是否忠实、引用是否准确）不在本脚本范围；需要时用真实 `chat` 回答
+ 较强 API 模型做 judge 单独评。

## ⚠️ 可答率口径（Answerable@10）——与上面的口径不同

上面两个指标的判据是"**指定的那一份** support 文件是否进 top-10"，会**系统性低估**
真实检索能力：库里常有多份文档都能回答同一个问题（例：多份劳动合同都含"提前三十日
书面通知"），找到任意一份即为成功，但旧口径判为 miss。

`scripts/eval/run_eval_answerable.py` 用 **LLM judge** 逐份判断 top-10 文档能否回答，
任一份可答即算命中：

```bash
python3 scripts/eval/run_eval_answerable.py <golden.jsonl> <data_dir> \
  --llm-url <URL> --llm-key <KEY> --llm-model <MODEL>
```

实测（88 题，judge=agnes-3.0-flash）：**可答率 87.50%** vs 旧口径 72.73%（+14.77pp），
差异主要在 `semantic` 类（45.45% → 78.79%）。

⚠️ **两点注意**：
1. **judge 只喂文档前 3000 字** → 长文档（`long_doc`）答案在深处时**假阴性**（该类别
   显示 40%，不代表检索失败）。长文档需改按 chunk/相关段落喂 judge。
2. **评测前确认 embedding/reranker 服务在运行**（`curl 127.0.0.1:8000/v1/models`
   返回 200）。服务未运行时向量通道静默关闭（输出 `sem=-`），结果不可比。

## 变更记录门禁

任何涉及检索/注入的改动（P1-A 两级漏斗、P1-B 懒嵌入、编号体系、chunk 切分、
budget 调整）合入前，必须跑本评测并对比基线。数字回落即回归。
