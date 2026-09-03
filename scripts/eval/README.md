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
  golden.jsonl     # 标注：每行 {"question": "...", "support_files": ["a.txt", ...]}
```

`support_files` 用 **basename** 匹配（不依赖绝对路径），支撑文件应能唯一对应
`docs/` 下的某个文件。

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

## 指标含义

- **Context Recall@10**：golden 标注的支撑文件里，有多少比例出现在注入 evidence
  前 10 条。抓"该有的材料没检索到"。
- **Success@10**：多少问句至少有 1 个支撑文件进前 10。抓"完全没检索到"。

生成层（回答是否忠实、引用是否准确）不在本脚本范围；需要时用真实 `chat` 回答
+ 较强 API 模型做 judge 单独评。

## 变更记录门禁

任何涉及检索/注入的改动（P1-A 两级漏斗、P1-B 懒嵌入、编号体系、chunk 切分、
budget 调整）合入前，必须跑本评测并对比基线。数字回落即回归。
