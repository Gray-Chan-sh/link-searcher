# 面向个人/小团队的本地文档 AI 问答（RAG）最佳实现调研报告

> **调研对象**：Link-Searcher（Tauri + Rust 本地全文搜索工具，正在加 AI 问答）
> **场景边界**：个人 / 小团队（1–10 人）；本地 / 私有文档；语料总量小（几千到几万条 chunk）；本地 embedding + 本地 LLM 或 API。
> **方法说明**：以下结论基于公开资料的深度检索与原文核实。**宁可引用少而准，不编造来源**。凡未能完整核实正文的来源均标注「未能核实」。观点性文章（尤其带具体数字的）以"某团队/作者实测"形式标注，便于读者判断可信度。

---

## 0. 最重要的一个前提：你的语料到底有多小？

在进入任何分块 / embedding / 重排细节之前，先回答一个"规模"问题。多个权威来源给出了一致而常被忽略的建议：

- **Anthropic 官方**（Contextual Retrieval 一文，2024-09）明确说：
  > "如果你的知识库**小于约 20 万 token（约 500 页材料）**，最简单且最好的方案是把整个知识库直接塞进 prompt，**根本不需要 RAG**。"
  他们同时指出：对于这种规模，配合 prompt caching 效果更好、成本更低。
  来源：https://www.anthropic.com/engineering/contextual-retrieval

  对 Link-Searcher 而言，若单用户文档总量折算后常低于 20 万 token，那么"全库进上下文 + 让模型自己找"是一个应当严肃考虑的基线方案（尤其当用户主要用 API 模型时）。全文搜索仍然负责"定位到文件"，问答则可以直接吃全文件。

- **"Karpathy LLM Wiki"模式**（2026-04 发布后大量传播）：对几百篇以内的个人文档，"编译为结构化 Markdown wiki（索引 + 概念页 + 双链）"往往优于向量 RAG；实测者称在几百个文件规模下"没有分块妥协、没有相似度≠相关度问题"。但它有代价：入库要用较强模型做编译、需要持续维护、超过约 150–200 页/文件后超出单模型上下文。
  来源（非官方解读，含实操数据）：
  - https://www.mejba.me/blog/karpathy-obsidian-rag-knowledge-base
  - https://www.kunalganglani.com/blog/llm-wiki-karpathy-local-knowledge-base

**对"小语料"的关键含义**：你的检索系统可以、也应该比大厂 RAG 简单得多——简单意味着更少故障点、更少过度工程。下面各节会反复回到这一点。

---

## 1. 检索策略（chunk 设计、混合检索、embedding 选型、是否微调）

### 1.1 共识

- **先选切分策略，再谈 embedding。** 多个来源强调："切分策略决定了检索器永远看不到什么"，其影响通常大于换 embedding 模型。来源：https://www.pinecone.io/learn/chunking-strategies/
- **固定大小切分是最合理的起点**，然后按需迭代。Pinecone 官方原文："Fixed-sized chunking will be the best path in most cases, and we recommend starting here and iterating only after determining it insufficient."（https://www.pinecone.io/learn/chunking-strategies/）
- **"一个 chunk 单独拿出来、人能读懂，模型才能用得上"**是通用的经验法则（同上来源）。
- **短小、自包含的文档可能根本不需要切分。** Pinecone 明确列出判断项："Small documents may not need to be chunked at all"。反过来，段落/句子级 chunk 利于检索精确定位，整篇文档级向量利于主题级匹配。来源：同上。
- **小语料上混合检索（BM25/词法 + 稠密向量）收益大、代价小。** Anthropic 官方实验：仅用 embedding 的 top-20 检索失败率 5.7%；加 BM25（Contextual BM25）后失败率降 49%（5.7%→2.9%）。原因：embedding 常漏掉精确标识符/型号/编号/缩写，而 BM25 擅长这些。来源：https://www.anthropic.com/engineering/contextual-retrieval
- **向量检索与 BM25 各自有"不可替代"的盲区**：稠密检索"聪明的语义"在未针对领域微调时未必强于 BM25；越界域（out-of-domain）时 BM25 常与稠密相当甚至更稳。来源：https://www.pinecone.io/learn/hybrid-search-intro/
- **BGE-M3 官方推荐流水线即"hybrid retrieval + re-ranking"**，且该模型同一套权重同时产出 dense / sparse(词法权重，类 BM25) / ColBERT 多向量三种表示。来源：https://huggingface.co/BAAI/bge-m3

### 1.2 分歧 / 相互冲突的观点

- **要不要"父文档检索（parent-document/small-to-big）"？**
  - 支持方：用小 chunk 检索保证精确定位、把命中 chunk 所在的更大父段/整文档交给 LLM，可显著改善答案完整性。LlamaIndex 官方文档即推荐"auto-merging / sentence-window"这类 small-to-big 检索。
  - 反方/提醒：对**本来就很小的个人文档**，chunk 与"父文档"往往没差别；引入层级只增加复杂度。Barking Iguana 的实操贴总结为："If documents are long and topically mixed, prefer semantic chunking… if records are short and self-contained…"——即切分方案应随文档形态而定。
  - 来源（未能完整核实正文，标题/摘要可证）：https://reintech.io/blog/implement-parent-child-document-retrieval-rag-systems ；https://barkingiguana.com/writing/cheat-sheet-rag-and-vector-stores/
- **"越界域时先训/微调 embedding vs 直接上混合检索"**：Pinecone 认为二者是互补路径——微调 embedding 需要标注数据，而个人场景拿不到；此时混合检索是现实解。（见上 1.1 末条，来源同 hybrid-search-intro。）

### 1.3 具体数字 / 阈值

- **chunk 常用量级**：Pinecone 建议从小（128–256 token）到大（512–1024 token）做对比实验；Anthropic 在 Contextual Retrieval 中称 chunk"usually no more than a few hundred tokens"，其示例为 800-token chunk；文档上下文示例 8k token。
- **chunk overlap**：LangChain 默认递归切分器常用 10–20% overlap；有实测称"按 section 切 + 100 token overlap"比"固定 500 token 硬切"好 5 个点左右（来源：https://www.luningqi.com/en/blog/rag-recall-engineering-tradeoffs —— 该文同时指出，表格类 PDF 经 OCR 损坏等"文档预处理"问题常比检索算法值 10–15 个点，是更大的瓶颈）。
- **Anthropic 实测 top-k 选择**：试了 5 / 10 / 20 个 chunk，**20 个效果最好**；但"更多未必更好，信息太多会干扰模型"。这印证了 top-k 与 chunk 大小需联合调优。来源：https://www.anthropic.com/engineering/contextual-retrieval

### 1.4 工具 / 模型清单（本地优先）

| 项 | 候选 | 说明 |
|---|---|---|
| Embedding（本地，Apple Silicon/CPU 可跑） | `bge-m3`（~560M，1024 维，8192 token，多语言含中英；dense+sparse+ColBERT 三合一） | https://huggingface.co/BAAI/bge-m3 |
| Embedding（本地，轻量） | `nomic-embed-text`（137M，Ollama 官方列示）；`mxbai-embed-large`（334M）；`all-minilm`（23M，最快） | https://ollama.com/blog/embedding-models |
| Embedding（API） | text-embedding-3-small/large、Cohere embed、Voyage | Anthropic 实验里 Gemini/Voyage embedding 表现最好（未测 OpenAI），供参考 |
| 本地向量存储 | SQLite + `sqlite-vec`；FAISS；ChromaDB；`usearch`；qdrant（本地版） | 详见第 5 节 |
| 词法检索 | SQLite **FTS5 / BM25**（Rust/Tauri 场景几乎零成本，天然契合 Link-Searcher 现有 SQLite） | 见 1.1 / 5 |
| Rust 生态 | `rust-bert` / `candle` / `ort`(ONNX) 可本地跑 bge / bge-reranker；`tantivy` 是 Rust 原生全文/BM25 引擎 | 未逐一核实 API 细节 |
| 上下文注入增强 | Anthropic "Contextual Retrieval"：切 chunk 前用 LLM 生成 50–100 token 的"chunk 语境说明"前缀，再 embedding + BM25 | 见上；对小语料也可手写规则（标题+文件路径即上下文） |

**关于"是否微调 embedding"**：
- 共识是：没有充足、有标注的领域数据时**不要微调**（Pinecone：越界域微调需标注大数据集；见 hybrid-search-intro）。
- 个人场景更现实的替代：换更强的通用模型（如 bge-m3）或做"上下文检索/语境增强"。
- 个别实测声称"把 nomic 换成 bge-m3 后 recall 从 0.45→0.95"（https://www.blog-des-telecoms.com/en/blog/embedding-bge-m3-rag-local-wazo/ —— 未能核实全文，存疑），至少说明**换模型比微调更可行**。

### 1.5 给 Link-Searcher 的具体建议

1. **不要为 1–10 人小语料引入重型 RAG 基础设施。** 复用现有 SQLite：FTS5/BM25 作为词法检索与向量检索并列。
2. **先测"整库进上下文"与"检索式"哪个更符合你的模型与延迟预算**；文档少的用户可直接给"问答基于你全部 N 篇文档"体验。
3. **默认配置**：结构感知切分（按标题/段落/列表，尊重 Markdown/HTML/PDF 结构）→ 兜底固定大小 ~512 token、overlap ~10–20%；对单个小文件（如 <1–2k token）整篇作为一个单元、不切分。
4. 中文场景优先 **bge-m3**（多语言 + 自带 sparse 权重可作 BM25 替代或补充）；纯英文/极轻量可用 nomic-embed-text。
5. embedding 不微调；用"chunk 前缀 = 文件路径 + 标题 + 邻近段落摘要"这类低成本语境增强替代。

---

## 2. 精排与质量（rerank、top-k、去重、分数融合）

### 2.1 共识

- **两阶段"先宽后精"是生产 RAG 标准范式**：召回（dense+BM25，可能几十到上百个候选）→ 精排（cross-encoder 或 LLM）→ 取 top-k 进 prompt。Anthropic 官方流水线即"初始检索 top-150 → rerank → top-20 进 prompt"。来源：https://www.anthropic.com/engineering/contextual-retrieval
- **cross-encoder 类专用 reranker 在质量/成本/延迟上全面优于 LLM 做 pointwise 打分**。ZeroEntropy 的 17 数据集基准：BGE-reranker-v2 NDCG@10=0.74、中位延迟 12ms、$2/1k 查询；而 Gemini Flash pointwise NDCG@10=0.68、185ms、$27/1k——**更贵 10 倍、更慢、反而更不准**。来源：https://www.zeroentropy.dev/articles/llm-as-reranker-guide
- **LLM listwise 重排是 LLM-rerank 唯一较有说服力的用法**：NDCG@10 0.74→0.78（+0.04）但要付出 ~9x 成本、~35x 延迟；只适合低并发、高价值场景。同上。
- **rerank 候选量建议 ≤50**；对 top-50 用 cross-encoder 精排到 top-10 是常见配置。同上 + AI/DE 的架构建议（https://ai-de.net/insights/decisions/enterprise-rag-002-cross-encoder-reranking ，未能核实全文）。
- **近重复/冗余文档会压低答案质量**：reranker 只按单条相关性打分，会把"5 份表达同一观点的文档"全排到前面，压制互补信息；MMR（最大边际相关）等可显式在相关性与多样性间权衡。来源：https://avchauzov.github.io/blog/2025/reranking-trap/
- **分数融合常用 RRF（Reciprocal Rank Fusion）**，因其不依赖跨检索器的分数可比性，被多篇文章推荐为混合检索融合默认项。（来源：https://www.digitalapplied.com/blog/hybrid-search-bm25-vector-reranking-reference-2026 等，未能逐篇核实正文；RRF 概念可上溯至 Cormack et al. 2009。）

### 2.2 分歧 / 冲突观点（重要）

- **"rerank 不是免费的午餐，甚至可能是负优化"**（The Reranking Trap，作者实测 deployed 系统）：
  - neural reranker 引入 **~200% 延迟惩罚**；
  - 系统性压低结果多样性；
  - **当 bi-encoder 检索质量已经很好时（简单事实型问题正确答案常已在第一位），rerank 无收益**；
  - "Garbage in garbage out"：rerank 无法找回未被召回的相关文档，只能重排已召回集合。
  - 作者给的建议：先做 embedding/检索适配，别默认加 rerank；高吞吐/亚 100ms 场景下 rerank 代价不可接受。
  来源：https://avchauzov.github.io/blog/2025/reranking-trap/
- **这与 Anthropic"rerank 总比没有好"（+67% 失败率下降）形成对照**——差异根源在于语料规模与候选数：大语料 + 候选 150 时 rerank 价值大；小语料 + 本就 top-10 够用 + 追求低延迟时，rerank 收益边际小。**对个人小语料，这可能是全报告最重要的"别过度工程"信号之一。**

### 2.3 具体数字 / 阈值

- 候选集：召回 50–150 → rerank → top-10~20 进 prompt（Anthropic top-20 最优；AI/DE 建议 top-50→top-10；ZeroEntropy 生产混合管线为 top-200 → cross-encoder top-20 → LLM top-10）。
- 本地 cross-encoder（bge-reranker 系列）单次打分在 CPU 上毫秒级（ZeroEntropy 测 12ms 中位，GPU）；BGE 官方建议对多语言/中英用 `bge-reranker-v2-m3`（0.6B）或 layerwise 加速版本。来源：https://huggingface.co/BAAI/bge-reranker-v2-m3
- rerank 对 N 个候选需 N 次前向，N=500 时 7B LLM rerank 延迟 ~1.15s（The Reranking Trap）。**个人场景不应让候选集超过几十个。**

### 2.4 给 Link-Searcher 的具体建议

1. **默认不做 LLM rerank**（pointwise 纯亏，listwise 延迟不可接受）；本地可选项是 **bge-reranker-v2-m3 / bge-reranker-base**（~0.1–0.6B，CPU 可跑），且只在召回质量确实成为瓶颈时启用。
2. **更便宜的"精排"替代**：把 top-k 从 5 提到 10–20、用"结构/文件去重 + 时间/相关性加权"，多数时候已够。
3. 融合用 **RRF**（简单、无需调跨系统权重）；如用加权和，权重按你现有搜索打分校准（见第 5 节）。
4. **入库即去重**：Link-Searcher 已有 MD5 去重（AGENTS.md 提到 indexer 做 MD5 去重），问答层还应做**近似重复 chunk 抑制**（同一文件相邻 chunk 同时命中时合并或降权），避免上下文被重复内容填满。

---

## 3. 查询理解（query rewriting / multi-query / HyDE / follow-up 改写）

### 3.1 共识

- **四类技术都是"在检索失败时才需要"的补丁，不是默认开启的功能。** 多个来源一致强调"按诊断出的召回失败类型路由选择，而不是全堆上"。
  来源：https://sincllm.com/blog/rag-query-rewriting-hyde-decomposition （未能核实全文）；https://www.bestaiweb.ai/when-to-use-hyde-vs-multi-query-vs-step-back-prompting-choosing-the-right-query-transformation-for-your-rag/ （未能核实全文）
- **query rewrite 对短查询（2–3 词）有用；对长自然语言问题几乎无益甚至有害。**（来源：luningqi 实测，见下）
- **HyDE 最不稳定**：有时 +8 分，有时负收益；在专用领域里 LLM 会"编造"看似合理的假设答案，反而带偏检索。（同上）

### 3.2 具体数据（某团队多项目实测，来源：https://www.luningqi.com/en/blog/rag-recall-engineering-tradeoffs ，基线 recall≈75%）

| 技术 | Recall 提升 | 额外延迟 | 说明 |
|---|---|---|---|
| Query rewrite | +4–6 pts | +400ms（1 次 LLM 调用） | 适合短查询 |
| Multi-query（3 条） | +6–10 pts | +600ms | 问题含糊时值（客服场景典型） |
| HyDE | +2–8 pts，可正可负 | +800ms | 最不稳定 |
| **混合检索（BM25+dense）** | **+8–15 pts** | **+50ms** | **"先开这个，近乎免费"** |

关键结论（原文）：
- "Turn on hybrid retrieval first. It is near-free… 100% of our RAG systems run with hybrid on."
- **"把所有技术叠一起反而更差"**——rewrite+multi-query+HyDE 同开时，检索集合发散、融合时噪声大于信号。稳定组合 = 混合检索（常开）+ multi-query（领域含糊时）。

### 3.3 分歧 / 冲突观点

- 教程类内容普遍把 multi-query / HyDE 当作"进阶必学技巧"；而实测型作者（如上）认为它们应"按需、克制地"使用。对**个人文档问答**，用户问题通常围绕自己文件里的事实，意图相对明确，多路查询的收益上限更低。
- 对本场景更相关的一个反共识：**follow-up 多轮对话**中，用户说"那个呢？""还有吗？"这类指代，需要轻量上下文改写（把上文问题+当前问题合成独立查询），这属于对话管理而非 RAG 增强，值得做；但可以用"最近一轮问题文本拼接"这类简单规则，不必上 LLM 改写。（此条为结合上下文工程通用建议的工程判断；来源参考 Anthropic context engineering 中"最小高信号 token"原则：https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents ）

### 3.4 给 Link-Searcher 的具体建议

1. **第一阶段：不做 query rewriting / multi-query / HyDE。** 先把"混合检索 + 合理切分 + 足够 top-k + 能答就答、答不了就明说"基线跑稳。
2. 记录检索失败案例，若出现"同义词/口语化问法命中不了"再考虑加一步：用**低成本本地模型做单次 rewrite**（如多语言短查询扩写），并加开关（设置页"增强检索"）。
3. **多轮对话做指代消解**：维护会话上下文，把"它/那个/上面说的"展开成完整查询；用规则或小模型皆可。

---

## 4. 生成（LLM 选择、context 注入、token 预算、引用机制、grounding 提示词、"答不了就明说"）

### 4.1 共识

- **context 是有限资源，越多不一定越好。** Anthropic 明确："Context must be treated as a finite resource with diminishing marginal returns"；transformer 注意力是 n²，长上下文稀释注意力——即"context rot / lost in the middle"。**目标不是塞满，而是"最小的高信号 token 集合"**。
  来源：https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
- **Lost in the Middle（经典论文）**：LLM 对长上下文中间位置的信息利用最差，首尾最好；因此检索结果排序（把最相关的放最前/最后）、控制上下文长度都直接影响准确率。论文：Liu et al., "Lost in the Middle: How Language Models Use Long Contexts", 2023。来源：https://arxiv.org/abs/2307.03172
- **引用让"可信可查"，是 RAG 答案的标配。** 引用三价值：信任（用户可核对）、调试（错了能查是检索还是生成）、合规。
  - RedHop 指南：推荐"**编号式行内引用 + 底部来源列表**"混合 UI（Perplexity / Claude 同款），并给出 4 步实现：给每个送入的 chunk 编号 → 提示词要求用 [N] 引用 → 校验输出里的 [N] 是否都在真实编号内（抓幻觉引用）→ 只渲染被真实引用的来源。
  来源：https://www.redhopai.com/guides/rag-citations/
  - Neel Mishra：同样给出"number chunks → 强制引用 → 正则校验 → NLI 逐句验证"的完整流水线，并给出量化说法："即便检索完美，GPT-4 级模型仍有 8–15% 的回答会编造细节；强制引用可把无据声称压到 3% 以下"（该数字为作者转述研究，未给一手论文链接，酌情采信）。来源：https://neelmishra.github.io/blog/mlops/rag/citation-grounding.html
- **"只基于给定资料回答"是 ground 的标准提示法**，但**提示词只是第一道防线**；强一致还需要：结构化上下文（XML/编号）、必要的输出约束、以及校验/回退。同上两来源 + Anthropic context engineering。
- **"资料不足就明说/拒答"优于硬编：** 多篇实务文章一致：让模型在找不到支持时直接说"根据现有文档无法回答"，并提供建议（如换关键词/放宽范围），比生成看似合理但无依据的答案更能建立信任。（综合 RedHop、Neel Mishra、Anthropic；另见 BooleanBeyond 概括性清单——未能核实正文：https://www.booleanbeyond.com/solutions/rag-ai-knowledge-systems/hallucination-reduction ）
- **本地小模型（7B–14B）做 grounding 是可行的，但别期望与旗舰 API 相同的遵循度**；需要更明确的格式约束（编号、JSON、少样本），且"引用格式错误率"会随模型能力下降而上升。（综合多个本地 LLM 指南；具体数字见 4.3。）

### 4.2 分歧 / 冲突观点

- **"chunk 越细引用越准 vs 越粗上下文越完整"**：Neel Mishra 指出粗 chunk（500+ token）让模型难以把引用精确到支撑句；细 chunk（100–200 token）提升引用精确性但吃上下文。→ 需要在 Link-Searcher 里按文档类型做权衡（细切分用于引用精度 + small-to-big 补全父上下文，是常见调和）。
- **引用数据流"来自检索器"（推荐）vs"来自 LLM"（不推荐）**：RedHop 明确推荐策略 A（检索器编号、LLM 只选号、程序校验），因为 LLM 自造引用（幻觉页码、串位）难以事后发现。此点无实质分歧，实务一面倒。来源：https://www.redhopai.com/guides/rag-citations/
- **本地模型是否胜任"引用编号"任务**：一部分本地模型指南（InsiderLLM 等，未能核实全文）声称 7B 级本地模型在严格引用上不可靠、建议用 API 或更大模型；但**任务本身（按给定编号输出 [N]）复杂度低**，配合强提示与校验，小模型的"引用错误"多数可被校验层拦下。工程判断：把"引用正确性"寄托在校验层而非模型智力上。

### 4.3 具体数字 / 阈值

- **token 预算**：无唯一答案，但可参照——chunk 数百 token；检索进上下文的 chunk 数 10–20 个（Anthropic top-20 最优）；给生成留足输出余量。若用户同时给全文（见第 0 节），要主动做截断/摘要/分片，控制总 prompt。
- **引用验证耗时**：NLI 模型逐条验证约 ~50ms/claim，5 句 8 引用约 400ms（本地可跑）——在桌面端属可接受的后台校验。来源：https://neelmishra.github.io/blog/mlops/rag/citation-grounding.html
- **本地 LLM 吞吐参照**（Kunal Ganglani 及社区综合数据，供预算参考）：
  - M3/M4 Max 64GB：14B Q4 约 25–40 tok/s；4090：14B 约 30–80 tok/s；纯 CPU（64GB RAM）跑 14B 约 10–25 tok/s。
  - 来源：https://studiomeyer.io/en/blog/local-llms-2026 （未能核实全文）；https://www.kunalganglani.com/blog/llm-wiki-karpathy-local-knowledge-base
- **常见本地模型选型区间**（多家本地指南，未能全部核实）：
  - 8GB 显存档：Qwen3-8B（或 Llama3.1-8B / Gemma）Q4；
  - 12–24GB 档：Qwen3-14B、Gemma3-12B；
  - 更高的遵循度/引用可靠性：往 32B+ 或 API 走。
  - **给 Link-Searcher 的建议**：把"本地 8B–14B"与"可插拔 OpenAI 兼容 API"做成两种 profile，默认按用户硬件自动推荐。

### 4.4 给 Link-Searcher 的具体建议

1. **Prompt 结构**（参考上下文工程）：`<system>` 规则 + `<documents>` 编号资料 + `<question>`。要求：只依据资料回答；逐句/逐 claim 用 [N] 引用；无依据时明说"文档中未找到"；不臆造引用。
2. **实现策略 A 引用管线**：检索层给 chunk 编号 → 生成后正则校验 [N] → 删除或标记幻觉引用 → 只展示真实引用的来源。这在 Tauri 前端很容易做成"可点击跳转到原文高亮"。
3. **"只基于资料回答"对本地小模型要配验证**：可选本地 NLI/HHEM 小模型（如 Vectara HHEM-2.1-open，T5 级，CPU 快）对回答做 faithfulness 抽查，低分时给 UI 打"低置信"标，而不是阻塞。
4. **答不了时的 UX 文案**：明确给出"未在 N 篇文档中找到答案 + 建议换词/放宽范围 + 一键切回关键词搜索"。这与 Link-Searcher 现有全文搜索天然衔接。
5. 对话历史别无限累积：按 token 预算做最近 N 轮 + 摘要压缩（Anthropic 的 compaction 思路），见 4.1 上下文工程来源。

---

## 5. 性能（延迟预算、小语料向量检索、缓存、流式、并发、显存/量化）

### 5.1 共识

- **小语料下，向量检索几乎不需要"近似最近邻（ANN）索引"。**
  - Faiss 官方指南：**"如果你只做少量搜索（~1000–10000 次），索引构建时间无法被摊薄，直接暴力计算（Flat）最划算"**；并明确 **HNSW 适合"数据集小、内存充足"**，但 HNSW 只支持顺序添加、不支持删除——对"文档增删改"频繁的桌面工具是硬伤。
  来源：https://github.com/facebookresearch/faiss/wiki/Guidelines-to-choose-an-index
  - 推算：几千到几万条 1024 维向量 = 约 40MB–400MB（fp32），线性扫描在 CPU 上是毫秒级；**完全无需 IVF/HNSW**。数据规模到了百万级才谈 ANN。
- **embedding 分为"离线/一次性"（文档入库时）与"在线/每次查询"（用户问题）**：文档向量入库后只算一次；查询向量每次现算。因此 embedding 推理延迟只影响"单次查询"（一次 ~几十 ms，本地小模型）。来源：https://apxml.com/courses/large-scale-distributed-rag/chapter-7-performance-tuning-benchmarking-distributed-rag/rag-caching-mechanisms-layers （未能核实全文；概念常识 + Ollama embedding 文档佐证：https://ollama.com/blog/embedding-models ）
- **缓存是性价比最高的优化之一**：
  - 查询 embedding 缓存（重复/近似问题直接复用）；
  - 语义缓存（按 query 向量相似度命中整条 检索+答案，需设阈值防误命中；有文章称相似度阈值 0.95 命中即返回缓存，成本降 ~80%——https://systemsbyakshay.substack.com/p/rag-latency-optimization-a-practitioners ，未能核实全文）；
  - LLM 响应缓存（相同问题直接复用）。来源汇总：https://github.com/pr0mila/Stop-the-Lag-A-Practical-Guide-to-RAG-Latency-Optimization （未能核实全文）
- **首 token 延迟决定"体感"**：本地 LLM + 流式输出，用户感知的是首 token 时间而非总时间；因此检索（~ms）与 prefill（受上下文长度影响）是优化重点，decode 速度（tok/s）决定总时长观感。
- **并发**：1–10 人桌面工具并发极低，但 Tauri 主进程不该被阻塞——推理应放后台线程/独立进程（AGENTS.md 也要求 async 命令避免阻塞事件循环）。

### 5.2 分歧 / 冲突观点

- **"小语料直接全量线性扫描 vs 用 HNSW"**：多数建议线性扫描（实现简单、精确、免维护）；个别向量库营销材料倾向推荐 ANN。工程上：**几万条内线性扫描 + 精确结果，胜于 ANN 的召回损失**，只有当文档量级涨到几十万+才需重审。来源见上 Faiss 指南（该页同时给 HNSW 的适用条件）。
- **"要不要为小语料上专用向量库"**：一派认为 ChromaDB/qdrant 等省事；另一派（如 Karpathy wiki 传播者）认为几百篇规模连向量库都不必——见第 0 节。Link-Searcher 已有 SQLite，**加 sqlite-vec 或直接用 SQLite BLOB 存向量 + 自己算余弦**都可行，避免引入新服务进程。

### 5.3 具体数字 / 阈值

- 端到端延迟预算（供设计目标）：
  - 检索（混合 + 可选本地 rerank）：目标 **<100ms**（本地小语料完全可达；rerank top-50 CPU 也就数十 ms 级）。
  - 本地 LLM 首 token：取决于 prefill 与硬件，通常 **数百 ms–数秒**；API 模型通常 <1s。
  - 本地 7B–14B decode：10–80 tok/s（视硬件/量化，见 4.3）；一条 200–400 token 的回答约 5–30s。**必须流式**，否则体感不可接受。
- 向量内存：1024 维 fp32 ≈ 4KB/向量；1 万 chunk ≈ 40MB——可全驻内存或 mmap。
- **量化**：本地 LLM 用 Q4_K_M 是性价比甜点（8B 约 5GB，14B 约 9GB 内存/显存）；embedding/reranker 模型小（0.1–0.6B），fp16/ONNX 即可，无需量化。来源（LLM 量化与显存，未能核实全文）：https://www.kunalganglani.com/blog/llm-wiki-karpathy-local-knowledge-base

### 5.4 给 Link-Searcher 的具体建议

1. **向量检索 = 内存内线性扫描**（或 sqlite-vec 的 flat 索引），不要上 HNSW/IVF。理由：精确、免训练、支持增删改、实现简单。到几十万 chunk 再升级。
2. **三层缓存**：① 查询 embedding 缓存（LRU，key=查询文本归一化）；② 高频问题语义缓存（可开关）；③ 相同 prompt 的 LLM 响应缓存。后两者注意"文档更新后缓存失效"。
3. **本地 LLM 常驻一个后台推理服务**（Ollama/llama.cpp 子进程即可，Tauri 通过 HTTP/stdio 调用），避免每次问答冷启动加载模型（加载 7B Q4 需数秒～十几秒）。
4. **流式输出 + 边生成边显示引用占位**（citation streaming，RedHop 提到 loading 占位模式），首 token 越快越好。
5. 并发按 1–2 个推理队列设计即可；不要为 1–10 人上重型并发。

---

## 6. 用户可感知性（UX）：引用、来源、兜底、RAG 与搜索的融合

### 6.1 共识（含产品实例）

- **主流产品（Perplexity / ChatGPT / Claude）的引用模式**：
  - Perplexity：行内编号标记（inline footnote），悬停显示来源卡，底部有 sources 栏；**3 个重排层筛来源，约访问 5–10 页只保留 3–4 个被引用来源**（多篇解析一致，未能核实一手来源：https://authoritytech.io/blog/how-perplexity-selects-sources-algorithm-2026 等）。
  - ChatGPT：底部来源卡（bibliography）为主，行内编号为辅。
  - **对桌面文档工具的建议是混合模式**：行内 [N] 可点击 → 跳转到原文文件 + 高亮段落；底部列出"本答案实际引用的来源（去重后）"。来源：https://www.redhopai.com/guides/rag-citations/
- **引用是 UX 信任的核心**，不是装饰：多个来源一致（RedHop、Neel Mishra、Inferensys："Poor citation displays … erode user trust, regardless of retrieval accuracy"——https://inferensys.com/blog/retrieval-augmented-generation-rag-and-knowledge-engineering/the-hidden-cost-of-ignoring-the-user-experience-in-rag-interfaces ，未能核实全文）。
- **答案应让用户看到"依据了哪几篇"**：展示"基于 N 篇文档（共 M 篇命中）"，并把来源按置信/相关性排序；低置信时给出兜底路径。
- **"先搜后问 / 搜问一体"是文档工具的自然形态**：ChatGPT/NotebookLM 也支持"从检索结果里选文件再问"。对 Link-Searcher，**关键词搜索已经存在**，问答应该是它的上层：用户在搜索结果里点"就这些结果提问"，或在全局问（自动先检索后生成）。来源：本报告工程判断 + RedHop 检索即引用前提（citations 的价值建立在检索正确的 chunk 之上）。

### 6.2 分歧 / 冲突观点

- **"显示来源数量/置信度" vs "过度展示技术细节吓到用户"**：产品向作者（Inferensys 等）主张透明展示；但也有观点认为对非技术用户，来源栏应"默认折叠、点开才见"。适合做成可配置。
- **无结果/低置信时的策略**：有主张直接"澄清式追问"（让用户补关键词/缩小范围）；有主张给"相近的其它问题/文档片段"引导。二者可结合：先显示"未找到充分依据"的说明 + 建议，再给检索命中的片段列表让用户自己判断。**给搜索兜底是共识**——RAG 答不了，就把话题交还给"你本来就有的高质量全文搜索"。

### 6.3 给 Link-Searcher 的具体建议

1. 回答区：
   - 流式文本，行内 [N] 上标 → 点击跳原文（打开文件并滚动高亮到对应段落；Link-Searcher 已有文件定位能力，只需把 chunk ↔ 文件内偏移存进元数据）。
   - 回答头部/尾部：**"回答基于 N 篇文档（来源按相关性排序）"**；来源条目显示：文件名、所在章节、命中片段预览、相关度分。
2. 引用真实性由管线保证（第 4 节策略 A），UI 只需忠实渲染；幻觉引用在生成后被剥离，用户看到的就是"可信来源"。
3. 无结果兜底：
   - 若检索 top-1 相似度低于阈值 → 不生成硬答，改为："在这些文档里没找到相关答案。建议换关键词 / 放宽日期 / 使用全文搜索"，并展示真实检索片段。
   - 提供"对本文件提问""对选中范围提问"等**范围限定入口**，减少跨库幻觉。
4. RAG 与现有搜索的融合（推荐交互）：
   - 入口 A：搜索页结果右上角"问 AI"（基于当前结果集）。
   - 入口 B：全局问答框（自动混合检索，答完可点引用回到搜索）。
   - 设置页可切：模型 profile（本地/API）、检索开关（BM25/向量/混合）、top-k。
5. 把"回答是否可查证"做成产品卖点文案（本地、私有、可点击溯源），与纯关键词搜索形成互补而不是替代。

---

## 7. 评测（小语料下如何评估问答质量）

### 7.1 共识

- **RAGAS 是事实标准之一**（由 Exploding Gradients 开源；现属 Hugging Face 生态的评测套件）。核心指标分"组件级"与"端到端"：
  - **Faithfulness（忠实度）**：回答中的声明有多少能被检索上下文支撑。公式 = 被上下文支持的声明数 / 总声明数（0–1）。**这是 RAG 最该盯的指标之一**（抓幻觉）。
  - **Answer Relevancy（答案相关性）**：回答对问题的切题程度。
  - **Context Precision**：检索出的上下文里"真正相关"的比例（重排后越靠前越相关则分越高）。
  - **Context Recall**：黄金参考里的事实有多少被检索上下文覆盖（抓漏检）。
  来源：https://docs.ragas.io/en/stable/concepts/metrics/available_metrics/ ；Faithfulness 计算细则：https://docs.ragas.io/en/stable/concepts/metrics/available_metrics/faithfulness/
- **先跑基线、再谈高级优化**：多篇实践建议——先用"向量 + BM25 + 合理切分"跑出基线（含检索与生成两个层面的指标），再逐个实验 chunk / 模型 / rerank / 查询改写，用同一 golden set 对比，避免"叠了一堆技巧却不知道哪个有效"。来源：https://www.luningqi.com/en/blog/rag-recall-engineering-tradeoffs
- **Golden set（金标集）规模建议**：Neel Mishra 建议 50–100 条 QA 对由人工标注；且 golden set 应包含"无法回答/对抗性"用例（AppScale 文章建议含 unanswerable/adversarial cases——https://appscale.blog/en/blog/rag-evaluation-architecture-faithfulness-groundedness-retrieval-metrics-2026 ，未能核实全文）。
- **指标进 CI / 每次改动重跑**：多篇一致——每次改 prompt、换 embedding、改分块都应重跑同一评测，像单元测试一样对待。（dev.to Anna Danilec：https://dev.to/anna_danilec/rag-evaluation-with-ragas-measuring-faithfulness-context-precision-and-recall-in-production-31hj ，未能核实全文）
- **人类评测不可省**：自动指标（尤其 LLM-as-judge）有偏差，需抽样人工复核；对小团队，10–30 个精心挑选的真实问题的人工盲测往往比大而全的自动集更早发现问题。

### 7.2 分歧 / 冲突观点

- **LLM-as-judge 是否可靠 / 用哪个模型做 judge**：RAGAS 官方示例用 gpt-4o-mini 等做 judge；实务界对"用小模型 judge 大模型 / 本地模型 judge"存疑（judge 本身会幻觉、会偏袒更流畅的答案）。建议：评测 judge 用相对强的模型（API 最好），与"产品运行时模型"解耦。来源：Ragas docs（如上）；实践观点：The Reranking Trap 作者亦提醒"以指标为导向要小心过拟合指标"。
- **在小语料上，检索指标（context precision/recall）比生成指标更重要还是相反**：多数实务者认为 **faithfulness + context recall 是最具信号量的两个**（dev.to 作者；Neel Mishra 亦把 citation/faithfulness 作为核心）；检索漏检（recall 低）是"改不动生成"的根因，应先修。
- **是否值得搭完整 RAGAS 管线**：对 1–10 人桌面工具，完整 RAGAS（含 judge 调用成本与维护）可能过重；轻量替代 = 手工 golden set + 用 API 模型批量打分脚本 + 少量人工抽查。**这是本报告认为最符合场景的取舍。**

### 7.3 给 Link-Searcher 的具体建议（可落地的最小评测方案）

1. **建 golden set（建议 ≥30 条，来源多样）**：从真实用户/测试文档中抽 30–60 个问题，覆盖：事实抽取、跨文件综合、文件名/编号精确匹配、"文档中不存在"（拒答）、近义词表述。每个问题标注：期望答案要点、支撑文档 id、是否可答。
2. **离线评测脚本**（随 repo 跑，改检索/提示词/模型后必跑）：
   - 检索层：`Context Recall@k`、`Context Precision@k`、`nDCG@k`（k=10 即可，不必 k=20）。
   - 生成层：**Faithfulness**（必测）、Answer Relevancy（次选）。用 API 强模型当 judge；预算敏感时只跑 golden set。
3. **人工快检**：每次改动后人工看 5–10 条（尤其引用是否正确、拒答是否恰当）；这是发现"自动指标骗人"的最短路径。
4. **把"引用可点击、可溯源"也纳入评测**：检查回答中 [N] 是否都指向真实且支撑该句的 chunk（Citation Precision/Recall，见 Neel Mishra 一文），桌面工具的差异化竞争力正在于此。

---

## 8. 最关键的 10 条可执行建议（给 Link-Searcher）

1. **规模决定架构，先量化再选型**：给用户文档总量做分级——A 级（<~20 万 token / 约 500 页，Anthropic 口径）优先"整库进上下文 + 全文搜索定位文件"；B 级（更大）走下面的 RAG 管线。别默认所有用户都需要 RAG。
2. **混合检索是性价比之王**：BM25（FTS5/词法，你已有 SQLite 基础）+ 本地稠密向量，RRF 融合；比任何 query 改写都先做这个（实测 +8–15 recall、仅 +50ms 延迟）。中文/标识符/型号类查询尤其依赖词法腿。
3. **切分从"结构感知 + 固定大小兜底"开始**：按标题/段落/列表切（Markdown/HTML/PDF 结构你都已有提取管线），兜底 ~512 token、overlap 10–20%；小文件整篇入库不切；chunk 前缀自动带上"文件路径+标题"低成本语境增强（Contextual Retrieval 的廉价版）。
4. **embedding 选 bge-m3（本地、多语言、带 sparse 权重），不微调**；纯英文轻量场景可 nomic-embed-text。API 选项（text-embedding-3/Cohere）做成可插拔 profile。
5. **默认不做 LLM rerank；本地 rerank 用 bge-reranker-v2-m3 做可选增强**。小语料先验证"top-k 从 5 提到 10–20 + 去重 + 排序"是否已够，再决定要不要 rerank（注意 rerank trap：200% 延迟、多样性下降、对已够好的检索无增益）。
6. **top-k 设 10–20，token 预算收敛**：检索进上下文的文本总量控制在几百到 ~2–4k token 区间，按模型上下文留足输出余量；最相关 chunk 放最前（Lost in the Middle）。不要贪多。
7. **引用用"策略 A"（检索器编号 + 生成校验）做成硬管线**：chunk 编号 → 提示词强制 [N] → 正则校验剥离幻觉引用 → UI 渲染可点击来源。行内 [N] + 底部"基于 N 篇文档"来源列表。这是你相对通用 chat 的差异化护城河。
8. **答不了就明说 + 给搜索兜底**：低置信不硬答；给"未找到 + 换词建议 + 一键回到全文搜索"。UX 上提供"对当前搜索结果提问/对单文件提问"的范围限定入口。
9. **性能按"本地线性扫描 + 三层缓存 + 常驻推理 + 流式"设计**：向量全内存线性扫描（几万 chunk 毫秒级，别上 HNSW/IVF）；embedding/高频问答缓存；本地 LLM 用 Q4 量化并常驻（Ollama/llama.cpp 子进程），首 token 优先、全程流式。本地 8B–14B（Qwen/Llama/Gemma 系）与 API profile 并存。
10. **建 ≥30 条 golden set 并固定评测流程**：核心指标 Context Recall@10 + Faithfulness（judge 用较强 API 模型）；改 chunk/embedding/prompt/模型必重跑；每次人工复核 5–10 条（重点看引用正确性与拒答质量）。自动化指标只是过滤器，人工是最终裁判。

---

# 第二部分：对照 Link-Searcher 现状的差距分析与实施路线图

> ⚠️ **规模修正（2026-09-02，实测）**：第二部分最初按"1 万级中短文档"假设撰写；经用户澄清并实测真实库后，规模与推理环境如下，**下文第 9.1 节末尾的"规模参照"与第 10 章路线图已按此修正**，其余逐条对照（✅/❌）不依赖规模、仍成立。
>
> **实测规模（com.link-searcher.app 库）**：
> - 22,441 active 文件；总内容 **2.65 亿字**；平均 1.2 万字/文档；**最长 667 万字**。
> - 分档：`<1k` 5554 (25%) · `1k-10k` 10845 (50%) · `10k-50k` 4187 (19%) · `50k-200k` 1053 · `200k-1M` 77 · `>1M` 5。即 **75% 文档 ≤1 万字**，但尾部 82 篇超 20 万字、5 篇超百万字。
> - 另一库（/Volumes/Data/index，1.17 万文件）：doc_chunks 已 **12.5 万行/跨 1195 长文档**，chunk_embeddings 仅 1154（**回填滞后 ~100 倍**）。
> - 推理环境（用户确认）：本地 MLX `localhost:8000`——LLM `Qwen3.6-35B-A3B-MLX-4bit`；**实际生效 embedding 为本地 ONNX `local:bge-large-zh-v1.5`**（1024 维；配置里的 bge-m3-mlx 未启用）。
>
> **规模对结论的关键影响**：chunk 量级已是 12.5 万–50 万+，全表暴力余弦不再免费（见 9.1 末）；**检索必须走"文档级粗筛 → 命中块精检"两级漏斗**，而非全库 chunk ANN 或全库 chunk 暴力；文件级向量（前 2000 字符）在 667 万字文档前失效，检索单元必须是块；12.5 万块对 1154 向量的滞后要求**块级懒嵌入**；5 篇 >1M 文档使**结构感知切分成为必需**（而非原 P2 可选）。

> 本部分把第一部分的业界共识逐条对到 Link-Searcher 的真实代码上。核对方式：直接阅读 `src-tauri/src/` 与 `src/` 关键路径 + 检索文档（docs/08-ai-features.md、CHANGELOG.md），非仅凭二手描述。所有"现状"描述均给出**文件:行号**或可复现的检索命令，便于复核。
> 结论先行：Link-Searcher 的 RAG 管线**骨架已经踩在业界共识上**（混合检索、小文件不切分、引用编号、范围控制都在）；主要差距在"**答不准时怎么办**"（硬答/幻觉引用/静默截断）、"**规模适配**"（全表暴力余弦在 12.5 万+ chunk 下不再免费、块级嵌入严重滞后、文件级向量对超长文档失效）和"**完全缺失评测基线**"。

## 9. 对照 Link-Searcher 现状的差距分析

### 9.1 现有 RAG 管线速览（生产链路）

生产链路是 `commands/ai.rs` 里的单体 `prepare_conversation_prompt`（约 L960–1508），**不是** `ai/skills/*` 脚手架（`prepare_conversation_prompt_pipeline` 标了 `#[allow(dead_code)]`，未接生产——见 L1512-1520）。一轮问答的数据流：

```
用户输入（+@引用 chips + 检索范围 + strict/full_recall 开关）
 → ① 查询改写 rewrite_query(L514 规则/指代消解) + llm_rewrite_query(L597, 5s 超时降级, skip_llm_rewrite 可关)
 → ② 范围解析（L1043-1134）：dir_config 精确→dir_ids；文件路径→file_id；LIKE 回退；父吞子 merge_scope_prefixes(L1109)
 → ③ extract_retrieval_keywords(L658, jieba, MAX_KEYWORDS=3) → bm25_query = kws OR 连接(L1183-1187)
 → ④ 三路检索（L1196-1224）：
      a. BM25 bm25_relevant_hits(L827, Tantivy, top = max(total_files,500))
      b. 向量 vector_scan_with_query_emb（L1215, 阈值 0.65）+ chunk 级（阈值 0.55）
         —— 仅 embed 一次两通道共享（L1211-1213）；有锁定范围（@文件）时整段跳过（L1205-1206）
      c. path_match_files（SQL LIKE 文件名）
     三路并集 → weighted_mix/semantic_fuse（L730/749, score = w·cos_norm+(1−w)·bm25_norm）
 → ⑤ 旧来源保留（非 strict 时 ≤3 份 from_history）
 → ⑥ Layer 0 引用文件均摊预算注入（L1384-1388, 共 CONTEXT_BUDGET/3）+ Layer 1 命中注入（≤30 份, L1319/1355）
 → ⑦ strict 拒绝（L1469-1482）：missing mention / 无内容 / context 空 → 报错返回；**非 strict 无此检查**
 → ⑧ context = docs.join 后 truncate_text(max_context_chars)（L1466, 关 full_recall=50k, 开=140k, L1465）
 → ⑨ system prompt（"仅基于材料回答…标注 [N]" L1483）+ user_msg（@mention→[N] 替换 L1497-1499）
 → ⑩ chat_stream → ai-chunk 事件流式 → auto_cite(L1562) 兜底补 [N] → ai_events 落库
```

规模参照（**实测 2026-09-02**）：最大库 com.link-searcher.app 有 22,441 个 active 文件、2.65 亿字，其中 75% ≤1 万字、82 篇 >20 万字、5 篇 >100 万字（最长 667 万字）；另一库 /Volumes/Data/index 的 doc_chunks 已达 **12.5 万行**（跨 1195 个长文档），但 chunk_embeddings 仅 1154（回填滞后 ~100 倍）。**量级= chunk 12.5 万–50 万+，全表暴力余弦不再免费**（见下第 10 章 P1 规模适配）。

### 9.2 与业界实践逐条对照

#### ✅ 已符合（保留，别为了改而改）

| # | Link-Searcher 现状 | 业界共识（第一部分锚点） | 结论 |
|---|---|---|---|
| 1 | 三路检索：BM25 + 文件级向量 + chunk 级向量，加权融合（ai.rs L1196-1224） | 混合检索是性价比之王（§1.1, +8–15 recall / +50ms；Anthropic BM25 补 embedding 失败率 -49%） | **已做到位**。中文/编号/型号类查询靠 BM25 腿，语义靠向量腿 |
| 2 | ≤1 万字符小文件整篇直存不切块（chunks.rs CHUNK_THRESHOLD=10_000）；>1 万字符按 1500 字 + 200 overlap、句界对齐切块 | §1.1 Pinecone"小文档可能根本不需要切分"；固定大小切分是合理起点 | **符合**。1500 字对中文 ≈1500 token（中英混合文档会略少），略高于业界 128–1024 token 常见上限但可接受——且实际注入用"词重叠选块 + 预算截断"补偿 |
| 3 | 引用策略 A：检索层给 chunk/文件编号 → 提示词强制 [N]（L1483）→ 前端 `[N]` 链接跳原文（ChatPanel `#ref:`） | §4.1 RedHop/Neel Mishra：检索器编号 + 强制引用 + 校验，是唯一推荐管线 | **骨架正确**，但校验层缺失（见 ❌#2） |
| 4 | 检索范围控制：@文件/@目录、scope 快照、父吞子、strict 模式、跨轮保留（docs 08 §步骤4） | §6.1 NotebookLM/Perplexity"范围限定再问" | **已做到位**，是差异化优势 |
| 5 | 查询改写克制：规则指代消解（rewrite_query）+ 可选 LLM 改写（5s 超时降级），无 multi-query/HyDE | §3.1"叠满反而更差"；只对短查询/指代有用 | **符合"克制"原则**，且已有降级 |
| 6 | 多轮历史注入但截断 500 字/条（L1488-1490） | §4.1 别无限累积上下文 | **符合** |

#### ❌ 缺口（按优先级排，详见 §10）

| # | 缺口 | 现状（文件:行号） | 业界锚点 | 影响 |
|---|---|---|---|---|
| 1 | **0 命中/低置信时仍硬答** | strict 关闭时 context 为空也照常走 LLM（拒绝逻辑仅在 strict_docs 内 L1469-1482） | §4.1/§6.2"资料不足就明说"优于硬编；Perplexity 低置信给兜底 | 检索不到时模型自由发挥 → 幻觉答案，最伤信任 |
| 2 | **引用无白名单校验（幻觉引用没被剥离）** | auto_cite(L1562) 只"补 [N]"不"校验剥离"越界 [N]；前端只渲染、不过滤 | §4.2 RedHop"策略 A 必须程序校验，LLM 自造引用事后难发现" | 回答里出现不在材料里的 [N] → 用户点开是错误来源 |
| 3 | **长文档注入从头部截断，静默丢尾部** | context = truncate_text(50k/140k, L1466)；非 full_recall 时 BM25 命中 ≤30 份且每份按预算截断（L1355） | §4.1 Lost in the Middle；截断应"可感知"而非静默 | 长文档尾部结论/关键段落静默丢失，用户无感 |
| 4 | **文件级向量只编全文前 2000 字符** | embed 入口统一截断 `truncate_for_embed`（ai/mod.rs L169-176, EMBED_MAX_CHARS=2000）；文件级向量因此只编码文档头部 | §1.1（chunk 粒度决定检索上限） | 长文档中后段细节在文件级通道无信号（已靠 chunk 向量缓解，但头部偏好仍在） |
| 5 | **向量检索=每轮全表拉 SQLite BLOB 暴力余弦** | db/tracker.rs get_all_embeddings/get_all_chunk_embeddings → ai/mod.rs vector_full_scan 余弦 | §5.1 Faiss"少量搜索直接 Flat"；1 万向量全内存毫秒级 | 1 万级量级下 release 检索尚可用（debug dry-run 全流程 ~1m、release ~14s，大头是本地 BGE 推理），但每轮全表解码 + 无内存驻留，随文档增长线性劣化，且与 embedding 延迟叠加 |
| 6 | **查询 embedding 无缓存** | 每轮问答现算查询向量（L1213）；本地 BGE debug 下单次 ~85s（CHANGELOG），有 5s 超时保护 | §5.1 查询 embedding 缓存是最便宜的优化之一 | 本地模型下每轮都付 embedding 延迟 |
| 7 | **Web 模式无 ai-progress 事件** | webapi/mod.rs BRIDGED_EVENTS 只含 ai-chunk/ai-done，不含 ai-progress | §6（可感知性：阶段进度是信任的一部分） | Web 端无"检索中/生成中"阶段反馈 |
| 8 | **完全无评测基线** | 无 golden set、无离线评测脚本；改检索/提示词无法量化对错 | §7 RAGAS；"先跑基线再谈优化"；golden set ≥30 条 | **最关键的缺失**：现在无法证明"改完更准了" |
| 9 | **chunk 无结构/语境** | chunks.rs 纯字符窗口切块，无标题/章节/页码信息；仅打印 `（第{start}-{end}字）` | §1.5 Contextual Retrieval 廉价版（路径+标题前缀） | 检索/引用的定位精度有上限；无法跳原文段落 |
| 10 | **ai/skills/* 与生产单体双份实现** | prepare_conversation_prompt_pipeline #[allow(dead_code)]（L1512）；行为与单体有漂移 | §（工程收敛） | 后续改哪层容易错；Skill 版已退化 |

#### 🚫 明确不做（避免过度工程；第一部分已有量化论据）

| 候选方案 | 为什么不做 |
|---|---|
| 全库 chunk ANN/HNSW/IVF | 两级漏斗（P1-A：文档级粗筛→命中块精检）替代全库 chunk 索引——HNSW 不支持删除（Faiss wiki §5.1），桌面增删改场景是硬伤；且 50 万 chunk 向量 2GB 级不配常驻。文档级 2.2 万向量线性扫描仍够快，无需 ANN |
| LLM rerank | pointwise 贵 10x、慢 15x 还更不准；listwise 延迟不可接受（ZeroEntropy §2.1）；检索已够好时 rerank 无益（Reranking Trap §2.2）。仅在两级漏斗粗筛 top-50 阶段可选（P2-4） |
| multi-query / HyDE | 叠满反而更差（luningqi §3.2）；个人文档意图相对明确 |
| 换 embedding 到 bge-m3 | 实际生效已是本地 bge-large-zh-v1.5；重嵌 2.2 万 doc + 12.5 万块不值，收益不抵成本 |
| "整库进上下文"基线 | 22k 文件、单文档都可能超 20 万 token（Anthropic §0），彻底出局；仅在用户明确小范围/少文件时适用 |
| 本地 reranker（bge-reranker-v2-m3） | 可作为 P2 后期可选，但先验证两级漏斗粗筛 top-50 是否已够（§10.3 P2-4） |

### 9.3 关键缺口细节（附代码位置）

见上表。三个 P0 缺口的精确触发路径：
- **0 命中硬答**：`prepare_conversation_prompt` 返回的 `context` 为空/极短时，只有 `strict_docs==true` 才 `return Err(...)`（L1469-1482）；非 strict 时带着空 context 继续 → `chat_stream` 让 LLM 基于"材料：\n"硬答。
- **幻觉引用**：`auto_cite(L1562)` 的职责是"给没引用的句子补 [N]"；对 LLM 自己输出的越界编号（如 [99]）无白名单拦截。前端 `#ref:` 渲染基于 evidence 数组索引，越界编号会指向错误/不存在的来源。
- **静默截断**：`truncate_text`（L1466）对拼好的 docs 截断到 50k/140k 字符，被截掉的内容无任何提示（事件里只有 total_chars/truncated_to，L1501-1506）。

---

## 10. 分优先级实施路线图

> 排序逻辑：P0=直接影响回答正确性与信任（先做）；P1=性能与可感知性（性价比高）；P2=评测与工程收敛（是"准确度高"的度量根基，建议尽早但可并行）。每项含 **为什么 / 怎么做 / 怎么验**。
> 实施顺序建议：**P2-评测 应先于 P0 铺开**（哪怕先建 20-30 条 golden set + 记录当前基线），否则后续每个改动都无法证明"更准了"。但 P0#1/#2 属明显的正确性缺陷，也可不等评测直接修。

### 10.1 P0 正确性（推荐先做）

**P0-1 低置信/0 命中不硬答 + 给搜索兜底**
- 为什么：检索 0 命中/低置信时硬答 = 幻觉温床；"答不了就明说"是 RAG 信任基石（§6.2 共识）。
- 怎么做：
  - `prepare_conversation_prompt` 返回后、进 LLM 前：若 `all_hits` 为空 且非 strict → 不进 LLM，直接返回结构化"未找到依据"响应（含建议换词/放宽范围/一键回搜索）。
  - 加"低置信"判定：BM25 命中但混合分都低于阈值时（阈值用 golden set 标定，见 P2-1），回答前给"以下内容基于较弱匹配，请核对原文"提示（不改回答，只加透明度）。
  - 前端 ChatPanel：识别该结构化响应 → 显示"未在文档中找到答案"+ 建议动作按钮（换关键词 / 打开全文搜索），而不是空对话。
  - 涉及：`commands/ai.rs`（prepare_conversation_prompt 调用处 / 新增结构化结果）、`ChatPanel.tsx`、`api/files.ts`。
- 怎么验：构造 0 命中查询（乱码/不存在的词）→ 应返回"未找到"，不消耗 LLM；golden set 中"不可答"用例全部正确拒答。

**P0-2 引用白名单校验（剥离幻觉引用）**
- 为什么：RedHop"策略 A 必须程序校验"（§4.2）；越界 [N] 会指向错误来源，比没引用更伤信任。
- 怎么做：
  - `chat_stream`/`conversation_ask` 拿到完整回答后：正则提取所有 `[N]`，与本次注入材料的真实编号集合（evidence 里的合法 [N]）比对 → **剥离不在白名单的 [N]**（保留文字，删编号）。
  - `auto_cite` 补引前先做白名单校验（它当前假设所有 [N] 合法，L1562 附近）。
  - 加单测：回答含 `[99]`/`[abc]`/越界编号 → 校验后剥离；回答引用真实编号 → 保留。
  - 涉及：`commands/ai.rs`（新增 sanitize_citations）、`ai/mod.rs`（如 chat_stream 兜底）。
- 怎么验：单测覆盖越界/非数字/真实编号三态；golden set 中 citation-precision 用例通过（§7.4）。

**P0-3 截断可感知（静默截断 → 显式标记）**
- 为什么：Lost in the Middle + 静默截断 = 用户以为模型读完全文，其实尾部早被截掉（§4.1）。
- 怎么做：
  - `truncate_text` 在 Layer1/全长截断处加显式标记（如 `\n\n[…材料过长已截断，未包含 N 个字符…]`），让模型知道"材料不完整"，避免它把局部当全局下结论。
  - 前端证据面板显示"该文档被截断（原 N 字 / 注入 M 字）"。
  - 涉及：`commands/ai.rs`（truncate 调用处加标记 + 透出截断信息）、`ChatPanel.tsx`（证据面板显示）。
- 怎么验：构造 >50k 字长文档 + 问尾部事实 → 回答应明示"材料被截断/未包含"，而不是假装读过。

### 10.2 P1 规模适配（新规模下最关键的工程项）

> 原 P1-1 假设"1 万级向量，线性扫描够用"；实测 12.5 万–50 万+ chunk 后，**全表暴力余弦不再免费**（`get_all_chunk_embeddings` 每轮全表拉 BLOB 解码 + 余弦）。本节按真实规模重排。

**P1-A 两级检索漏斗（文档级粗筛 → 命中块精检）—— 规模适配核心**
- 为什么：75% 文档 ≤1 万字（文件级 2.2 万向量，线性扫描仍快）；真正长的只有尾部 82+5 篇。单次问答命中的块只来自少数几篇，**全库 50 万 chunk 扫描几乎全是浪费**。文档级粗筛（BM25 + 文件级向量，1 万级）→ 命中前几十篇 → 只对命中文档的 chunk 做块级余弦，把扫描量从"全库 chunk"降到"命中集 chunk"。
- 怎么做：
  - 检索顺序改为：① BM25 + 文件级向量粗筛 top-N（N≈50，全库 doc 级线性扫描内存驻留）→ ② 对粗筛命中的文档（去重后）载入其 doc_chunks + chunk_embeddings → ③ 块级余弦只在命中集内做，取 top-K。
  - 需要块↔文档映射：现 chunk_embeddings 按 (md5, chunk_index) 存，须能从"粗筛命中的 md5/file_id"反向取块（`get_chunks_by_md5` 批量）。这天然契合"命中才拉块"，避免 2GB chunk 全驻内存。
  - 涉及：`ai/mod.rs`（chunk_vector_scan 改为接收候选 md5 集）、`db/tracker.rs`（批量取命中集的块+向量）、`commands/ai.rs`（检索编排）。
- 怎么验：基准脚本对比单轮问答检索耗时；目标在 12.5 万块规模下**检索 <500ms**（含本地 BGE 查询嵌入）；命中集正确性用 golden set Context Recall 守护。

**P1-B 块级懒嵌入（chunk_embeddings 按需回填）**
- 为什么：实测 doc_chunks 12.5 万行、chunk_embeddings 仅 1154，**滞后 ~100 倍**。全量回填 12.5 万块 × 本地 BGE ≈ 不可接受的首次耗时（debug 下单块推理秒级）。当前 `run_backfill_chunk_embeddings` 每轮 500 md5 的幂等全量回填，在长尾超长文档下不现实。
- 怎么做：回填策略从"全量"改"按需"——只嵌**被检索命中且缺向量的块**（P1-A 第③步遇未嵌块时对该块补嵌并入库，或回退 BM25/词重叠）；已有向量优先。这样 12.5 万块中只有用户实际问到的子集被嵌入，首次使用即可用、随时间自然收敛。
  - 涉及：`commands/index.rs`（回填触发点）、`commands/ai.rs`（检索遇缺块补嵌）、`db/chunks.rs`（缺向量块查询）。
- 怎么验：新库/大文档索引后，问答能立即用（懒嵌），不需等待全量回填；补嵌幂等不重复。

**P1-C 文件级向量驻留内存 + 查询 embedding 缓存**
- 为什么：doc_embeddings 2.2 万行 ≈ 90MB（1024 维 f32），可常驻内存避免每轮 SQLite 解码；本地 BGE 查询嵌入每轮现算。
- 怎么做：doc_embeddings 首次查询后驻留（`Arc<RwLock<Vec>>`，文件增删改时失效重建）；查询 embedding LRU 缓存（key=归一化查询文本）。**chunk 向量不常驻**（2GB 级，靠 P1-A 命中才拉）。
  - 涉及：`ai/mod.rs`、`db/tracker.rs`（load_all_doc_vectors 一次载入）、`commands/index.rs`（失效钩子）。
- 怎么验：基准脚本对比单轮问答检索耗时；重启/索引变更后缓存正确失效。

**P1-D 结构感知切分（原 P2-3 上提）**
- 为什么：82 篇 >20 万字 + 5 篇 >100 万字，若用 1500 字盲窗硬切会切出大量语义不完整块（准确度上限被切分决定）；PDF 提取器已保留 form-feed 分页信号（extractor/pdf.rs L129），但 doc_chunks 没利用。结构感知是长文档问答准确度的**前置条件**，不是锦上添花。
- 怎么做：对 >50k 字长文档启用结构感知切分（识别 `\f` 页界/标题行/章节），chunk 前缀带"路径 + 页区间/章节名"；≤10k 短文档维持现状。**代价是重切分+重嵌入这些长文档**——但配合 P1-B 懒嵌入，只重嵌被命中的块，成本可控。
  - 涉及：`db/chunks.rs`（切分器升级 + chunk 存 prefix/页码）、`commands/index.rs`（重切回填）、`commands/ai.rs`（注入带页码）。
- 怎么验：golden set 中"长文档段落级引用"用例；重切仅影响 >50k 文档，短文档零变化。

**P1-2 文件级向量编码扩展 —— 降级为"不做"（说明）**
- 原方案（首+尾+中段池化）在真实规模下 ROI 低：文件级只承担"主题粗筛"（P1-A），细节召回交给块级精检；改文件级向量定义需全量重算 2.2 万 doc_embeddings，收益被 P1-A/P1-B 覆盖。**保留前 2000 字符嵌入即可**，长文档主题靠 BM25 文件名/正文 + 块级通道兜底。

**P1-3 Web 模式补 ai-progress**（不变）
- 为什么/怎么做/怎么验同原 P1-3：把 `ai-progress` 加进 `webapi/mod.rs` BRIDGED_EVENTS。

### 10.3 P2 评测与工程收敛

**P2-1 建 golden set + 离线评测脚本（"准确度高"的度量根基）**
- 为什么：现在无任何量化基线；"改完更准了"无法证明（§7 全节）。这是本路线图**最该先铺**的一项，且规模越大越需要（P1-A/B/D 每个都改检索行为，无评测无法验收）。
- 怎么做：
  - 建 `tests/golden/`（或 `scripts/eval/`）：30–60 条 QA，覆盖 事实抽取 / **超长文档中后段定位** / 跨文件综合 / 精确编号匹配 / **不可答（拒答）** / 近义词表述；每条标注支撑文件 + 期望要点。**语料用真实大库子集（固定快照目录 + 独立 data_dir），保证可重复、可进 CI**。
  - 离线评测脚本：**复用现成 `chat --dry-run` CLI（cli.rs L235-268）作检索桩**——对每条 golden 问题跑 dry-run，解析 `[N] bm25=.. sem=.. path=..` 输出比对支撑文件，算 Context Recall@10/Precision@10/nDCG@10，**无需新写 Rust 检索入口**。生成层 Faithfulness + Answer Relevancy 用真实 `chat` 回答 + 较强 API judge。
  - **评测脚本必须固定 embedding 配置**（dry-run 走 prepare_conversation_prompt，未配置 embedding 时向量通道静默跳过）——用 local:bge-large 固定，保证数字可比。
  - 每次改 chunk / embedding / prompt / 模型 / 检索逻辑（尤其 P1-A/B/D）必跑；结果写 `tests/golden/last_run.json` 可 diff。
  - 涉及：新增 `scripts/eval/`、固定测试语料目录、`docs/` 记录基线。
- 怎么验：跑通一次得到当前基线（如 Context Recall@10=X、Faithfulness=Y）；P0/P1 改动前后数字可对比。

**P2-2 ai/skills/* 与生产单体收敛**（不变）
- 同原 P2-2：删除脚手架（推荐）或切 Pipeline，需先确认 skills 单测价值。

**P2-3（原编号保留作引用）→ 已上提为 P1-D**

**P2-4（可选）本地 reranker（bge-reranker-v2-m3）作为可开关增强**
- 为什么：小语料先验证 top-k 已够（Reranking Trap §2.2），不默认上；在**两级漏斗的粗筛 top-N 阶段**（P1-A 第①步）若评测显示漏检，rerank 只对粗筛 top-50 精排，收益/延迟比远好于对全库做。
- 怎么做：作为设置页可选项，只对 BM25/向量粗筛 top-50 精排到 top-20。
- 涉及：新增 `commands/rerank.rs`（ort 加载 cross-encoder）、设置项。
- 怎么验：golden set Context Recall@10 提升 vs 延迟增加（粗筛 top-50 场景，目标 <200ms）。

### 10.4 明确不做清单（避免过度工程，按真实规模修正）

一句话版（修正后）：**不做全库 chunk ANN/HNSW**（两级漏斗替代——HNSW 不支持删除，桌面增删改是硬伤，且全库 chunk 向量 2GB 级不配常驻）；**不做全库 chunk 暴力扫描**（新规模下不再免费，P1-A 替代）；**不做 LLM rerank**（亏本）；**不做 multi-query/HyDE**（叠满更差）；**不换 bge-m3**（实际生效已是 bge-large-zh-v1.5，重嵌 2.2 万 doc + 12.5 万块不值）；**不做整库进上下文**（单文档都可能超 20 万 token）；**不改文件级向量定义**（P1-2 降级，见 10.2）。这些是"别过度工程"信号（§2.2 Reranking Trap、§5.1 Faiss、§3.2 叠满更差）在新规模下的直接推论。

### 10.5 验证方式汇总（每项改动的门禁）

| 项 | 门禁 |
|---|---|
| P0-1 | 0 命中/不可答用例全拒答，不消耗 LLM |
| P0-2 | 越界/非数字/真实编号三态单测；citation-precision 用例 |
| P0-3 | 长文档尾部事实 → 回答明示截断 |
| P1-A | 12.5 万块规模检索 <500ms；命中集 Context Recall 不降 |
| P1-B | 新文档问答即用（懒嵌），补嵌幂等 |
| P1-C | doc 检索耗时基准下降；缓存失效正确 |
| P1-D | 长文档段落级引用用例通过；>50k 文档重切，短文档零变化 |
| P1-3 | Web 端可见阶段进度 |
| P2-1 | 跑通基线，数字入文档，改动前后可 diff |
| P2-2 | cargo check + 现有单测全绿，手动问答无回退 |
| P2-4 | recall↑ 且延迟在预算内才保留 |

**建议落地顺序**：先 P2-1（建 golden set + 复用 `chat --dry-run` 记录基线）→ P0-1/P0-2/P0-3（正确性，与规模无关、改动集中、可单测）→ **P1-A 两级漏斗**（规模适配核心）→ P1-C（doc 向量驻留/查询缓存，为 P1-A 铺路）→ P1-B（懒嵌入）→ P1-D（结构感知切分，配合懒嵌入控成本）→ P2-2/P1-3。每一项独立 commit，用 P2-1 的评测门禁验收，避免大爆炸式重构。

---

## 附：主要来源清单（已核实正文者优先）

| # | 来源 | 类型 | 核实状态 | 在本报告中的用途 |
|---|---|---|---|---|
| 1 | Anthropic — [Contextual Retrieval](https://www.anthropic.com/engineering/contextual-retrieval) | 官方工程博客 | ✅ 全文核实 | 小语料阈值 20 万 token、混合检索 +49%、rerank +67%、top-20、chunk 数百 token |
| 2 | Anthropic — [Effective context engineering for AI agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents) | 官方工程博客 | ✅ 全文核实 | context 有限资源、注意力稀释、最小高信号 token、compaction/note-taking |
| 3 | Liu et al. — [Lost in the Middle](https://arxiv.org/abs/2307.03172) | 论文 | ✅ 摘要核实 | 长上下文中间位置信息利用差 → 排序/长度控制 |
| 4 | Pinecone — [Chunking Strategies for LLM Applications](https://www.pinecone.io/learn/chunking-strategies/) | 官方学习文 | ✅ 全文核实 | 固定切分优先、小文档可不切、128–1024 token 区间、语义/结构切分 |
| 5 | Pinecone — [Getting Started with Hybrid Search](https://www.pinecone.io/learn/hybrid-search-intro/) | 官方学习文 | ✅ 全文核实 | 越界域 BM25 价值、混合检索、alpha 权重 |
| 6 | Hugging Face BAAI — [bge-m3](https://huggingface.co/BAAI/bge-m3) | 模型卡 | ✅ 全文核实 | 多语言、1024 维、8192 token、dense/sparse/ColBERT、hybrid+rerank 建议 |
| 7 | Hugging Face BAAI — [bge-reranker-v2-m3](https://huggingface.co/BAAI/bge-reranker-v2-m3) | 模型卡 | ✅ 全文核实 | 本地 reranker 选型、0.6B、多语言、sigmoid 归一化 |
| 8 | ZeroEntropy — [Should You Use an LLM as a Reranker?](https://www.zeroentropy.dev/articles/llm-as-reranker-guide) | 技术博客（有基准） | ✅ 全文核实 | LLM pointwise 10x 成本更低准确率、listwise 9x/35x 延迟、混合三级管线 |
| 9 | Andrey Chauzov — [The reranking trap](https://avchauzov.github.io/blog/2025/reranking-trap/) | 实测博客 | ✅ 全文核实 | rerank 200% 延迟、多样性塌缩、已够好检索时无益 |
| 10 | Neel Mishra — [Citation and Grounding](https://neelmishra.github.io/blog/mlops/rag/citation-grounding.html) | 技术博客 | ✅ 全文核实 | 强制引用降幻觉、NLI 校验 ~50ms/claim、citation precision/recall、golden 50–100 |
| 11 | RedHop — [RAG citations](https://www.redhopai.com/guides/rag-citations/) | 技术指南 | ✅ 全文核实 | Perplexity/ChatGPT 模式、策略 A vs B、幻觉引用校验、流式占位 |
| 12 | Ragas — [Available metrics](https://docs.ragas.io/en/stable/concepts/metrics/available_metrics/) + [Faithfulness](https://docs.ragas.io/en/stable/concepts/metrics/available_metrics/faithfulness/) | 官方文档 | ✅ 全文核实 | 指标定义、faithfulness 公式与计算步骤、HHEM 本地判分 |
| 13 | luningqi.com — [RAG recall engineering tradeoffs](https://www.luningqi.com/en/blog/rag-recall-engineering-tradeoffs) | 多项目实测博客 | ✅ 全文核实 | 各技术 recall/延迟实测表、混合检索先开、叠满反而差、切分+5pts、预处理 10–15pts |
| 14 | Faiss Wiki — [Guidelines to choose an index](https://github.com/facebookresearch/faiss/wiki/Guidelines-to-choose-an-index) | 官方文档 | ✅ 全文核实 | 少量搜索用 Flat 直算；HNSW 适用小数据集但不可删除 |
| 15 | Ollama — [Embedding models](https://ollama.com/blog/embedding-models) | 官方博客 | ✅ 全文核实 | 本地 embedding 模型名与参数量（nomic 137M/mxbai 334M/all-minilm 23M） |
| 16 | Ragas 论文 — [Ragas: Automated Evaluation of RAG](https://arxiv.org/abs/2309.15217) | 论文 | ✅ 摘要核实 | RAGAS 定位 |
| 17 | BGE-M3 论文 — [M3-Embedding](https://arxiv.org/abs/2402.03216) | 论文 | ✅ 摘要核实 | bge-m3 技术出处 |
| 18 | Nomic Embed 论文 — [Nomic Embed](https://arxiv.org/abs/2402.01613) | 论文 | ✅ 摘要核实 | nomic-embed 出处 |
| 19 | HyDE 论文 — [Precise Zero-Shot Dense Retrieval without Relevance Labels](https://arxiv.org/abs/2212.10496) | 论文 | ✅ 摘要核实 | HyDE 出处（Anthropic 亦引用其评估"效果有限"） |
| 20 | GraphRAG 论文 — [From Local to Global: A Graph RAG Approach](https://arxiv.org/abs/2404.16130) | 论文 | ✅ 摘要核实 | GraphRAG 出处（用于说明"全局综述问题"与向量 RAG 互补，而非小语料必选） |
| 21 | Mejba — [Karpathy's Obsidian RAG](https://www.mejba.me/blog/karpathy-obsidian-rag-knowledge-base) | 实操博客 | ✅ 全文核实 | 几百篇个人文档下"编译型知识库"优于向量 RAG 的论点与代价 |
| 22 | Kunal Ganglani — [LLM Wiki Setup](https://www.kunalganglani.com/blog/llm-wiki-karpathy-local-knowledge-base) | 实操博客 | ✅ 全文核实 | LLM wiki 模式、~150–200 文件上限、本地模型能力下限（32B 才可靠） |
| 23 | Medium/Data Science Collective — [Reranking for RAG](https://medium.com/data-science-collective/reranking-for-rag-cross-encoders-llm-rerankers-and-latency-tradeoffs-cdeb69942ea2) | 技术博客 | ⚠️ 会员墙，仅核实开头 | 混合检索 top-20 → prompt 预算只容 top-3 时正确答案被挤掉的场景 |

> 说明：
> - "未能核实全文"的来源只用于支撑常识性/多源一致的论断，未用于承载关键数字；关键数字均来自上表 ✅ 项。
> - 检索过程中遇到的纯 SEO 站（大量含"2026"预测性日期的聚合文）已主动剔除，不以它们为据。
> - 报告中的"给 Link-Searcher 的建议"部分包含工程判断（非单一来源可证），已在正文标注。
