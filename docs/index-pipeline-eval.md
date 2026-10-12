# 索引流水线瓶颈评估：扫描 → 提取/OCR → 嵌入

> 评估时间：2026-10-12
> 触发问题：「居然是所有文件都 OCR/提取完成后再统一 embedding，两段大工程串行，总时长太长」
> 结论：**问题主因不是"阶段串行"，而是"回填调度 + 单把大锁"导致嵌入以「天」为单位积压；嵌入吞吐上限在远程网关（≈2,300 tok/s）。流水线穿插值得做，但收益大头来自"持续回填 / 拆锁 / 新内容优先"。**
> 本文只做评估，不改代码。

---

## 0. 环境与数据来源

| 项 | 值 |
|---|---|
| 数据目录 | `D:\index`（`config.json.data_dir`） |
| 日志 | `D:\index\app.log`，57 MB，532,485 行 |
| 数据库 | `D:\index\data.db`（≈1 GB，WAL） |
| 嵌入模型 | `aef9c491-…:bge-m3`（provider `oMLX-MacMini-M4`，**远程**，局域网 `192.168.1.100:8000`） |
| 扫描目录 | `D:/Syncthing/XC 小城`、`D:/Syncthing/工作日志` |
| 日志覆盖 | 2026-09-27 → 2026-10-12（含多次重启；期间嵌入模型从 `local:bge-large-zh-v1.5` 切到远程 `bge-m3`） |

**方法**：解析 `app.log` 的各阶段事件（`[SCAN]`/`[PERF]`/`[AI]`/`[INDEX]`）得到墙钟与速率；查询 `data.db` 的时间戳得到**端到端等待时长**（这一步不需要新实验）。

日志可靠性说明：532,485 行中 **205,478 行（38.6%）只有 `HH:MM:SS` 没有日期**，无法放入时间轴，因此"跨天精确对齐"能力有限；下文所有结论都只用带完整时间戳的行或 DB 时间戳。

---

## 1. 现状流程（实际代码路径）

```
trigger_scan (commands/index.rs)
  └─ scanner.full_scan / incremental_scan (scanner/mod.rs)
       └─ 遍历中每 250 个文件 flush_jobs() → indexer.batch_index()
            └─ 每 500 文件一块：
                 Phase 1  并行提取（batch_io_concurrency=8）：读盘+MD5+去重+
                          PDF/Office/OCR + 写 content_index / doc_chunks + mark_extracted
                 Phase 2  串行写 Tantivy + commit（本块立即可搜）
            末尾 → schedule_backfill_embeddings()   ← doc 向量
  └─ 扫描全部结束后（commands/index.rs:259）
       ├─ 线程 A: run_backfill_embeddings()        ← doc 向量（兜底，再来一遍）
       ├─ 线程 B: run_backfill_chunks()            ← 仅构建 chunk 文本
       └─ 线程 C: verify / integrity heal

启动路径 (lib.rs:566-577) 另有一路，**串行**：
  run_backfill_chunks() → run_backfill_chunk_embeddings() → run_backfill_embeddings()
```

关键实现点：

| 位置 | 行为 | 影响 |
|---|---|---|
| `commands/index/embeddings.rs:187` `schedule_backfill_embeddings` | 3 秒防抖；**运行中则直接丢弃本次调度**；守卫判断 `task == "backfill"` | 积压只能等"下一次 flush"或"扫描结束"再来一轮 |
| `embeddings.rs:23` `run_backfill_embeddings` | **一次性快照**：先取全量缺失行，再逐批嵌入，期间新入队的行不补 | 长扫描期间每轮只能覆盖当时已入库的文件 |
| `embeddings.rs:17` `BACKFILL_LOCK` | doc 与 chunk 回填**共用同一把锁** | 互相饿死 |
| `embeddings.rs:306` `run_backfill_chunk_embeddings` | **持锁循环到 backlog 清空**才释放 | 整库 chunk 补齐期间，doc 回填被阻塞数小时 |
| `embeddings.rs:214` `missing_embedding_rows` | 无排序 | 无法"新内容优先" |
| `db/chunks.rs` | `CHUNK_THRESHOLD=10,000`、`CHUNK_SIZE=1,500`、`OVERLAP=200` | 长文块数 ≈ 字符数 / 1,300 |

> 注意：`indexer.rs` 的**全文索引**早已是"分批提交、边提取边可搜"（Phase 2 每块 commit）。真正落后的只有**向量**。

---

## 2. 量化结果

### 2.1 提取（OCR / 解析）

- 全日志 `[PERF] 批索引总计` 合计：**12,015 文件 / 9.90 h** 墙钟（跨多次扫描与重启）。
- 单文件配对（`[INDEX] 开始` → `[INDEX] 提取文字`）9,538 个，合计 **59.12 h 线程时间**：

| 扩展名 | 线程耗时 | 数量 | p50 | p95 | max |
|---|---:|---:|---:|---:|---:|
| **pdf** | **57.96 h (98.0%)** | 4,345 | 5.0s | 188s | **4,922s** |
| jpg | 0.57 h | 2,230 | 1.0s | 2s | 13s |
| zip | 0.48 h | 13 | 18s | 1,199s | 1,199s |
| 其他（docx/xlsx/md/pptx/doc/txt…） | ≈0.1 h | 2,900+ | ≈0s | ≈0s | ≤1s |

- **PDF 里 >10s 的 1,729 个（疑似扫描件/大部头）占 55.90 h**。
- 最慢单文件：

```
4,922s  0.94MB   2 上海电科智能系统股份有限公司报告.pdf
3,024s  96.0MB   中华人民共和国民法典侵权责任编理解与适用…pdf
2,893s  93.6MB   中华人民共和国民法典婚姻家庭编继承编理解与适用…pdf
2,472s  27.7MB   组合 1.pdf
2,423s 370.0MB   中华人民共和国民法典合同编 理解与适用…pdf
```

→ **提取时间 = PDF（尤其扫描件 OCR），其他格式可忽略。** 一次 7,791 文件的全量扫描墙钟 ≈ **118 min**（历史 `[SCAN] 扫描完成 … in 7107262ms`）。

### 2.2 向量回填

- **doc 回填**：整个日志内 **63 次，合计 3.23 h**。
- **chunk 回填**：历史上只跑过 3 次很小的（889 / 75 / 37 块）。
- **2026-10-12 那次 chunk 回填**：`51299 个待补块`，`04:13:00 → 约 10:07`，**≈6 h**，速率：

| 指标 | 值 |
|---|---|
| 速率 p10 / 中位 / p90 | 96 / **142.2** / 160 块每分（中位 **2.37 块/s**） |
| 自适应计划 | batch `16 → 512`，并发 `1 → 8` |
| 峰值吞吐 | ≈ **2,100–2,700 tok/s**（全程平台期） |

→ **速率对 batch / 并发完全不敏感**：瓶颈是远程网关吞吐，不是本机、也不是"批太小"。

- 库现状（快照）：`file_tracking` active+indexed = **19,612**；`content_index` 16,350；`doc_chunks` **73,639**（来自 1,941 个 md5，平均 1,460 字/块）；`chunk_embeddings` 73,257；`doc_embeddings` 19,581。

### 2.3 端到端等待时长（本次评估的关键指标）

用 DB 时间戳计算 **"文件被索引 → 拿到向量"**，无需新实验：

**doc 向量延迟**（n=19,602）

| <1m | 1–10m | 10m–1h | 1–6h | 6–24h | **1–3d** | >3d |
|---:|---:|---:|---:|---:|---:|---:|
| 30.8% | 21.9% | 0.7% | 7.7% | 0.1% | **38.8%** | 0.2% |

p50 = 3.8 min，p75 = **3,478 min（58 h）**，p90 = 3,511 min，max = 348 h（14.5 天）。

**chunk 向量延迟**（n=1,931 篇长文）

| <1m | 1–10m | 10m–1h | 1–6h | 6–24h | 1–3d |
|---:|---:|---:|---:|---:|---:|
| 0% | 0.1% | 0.7% | 16.9% | **45.4%** | **36.9%** |

p50 = **801 min（13.4 h）**，p75 = 3,570 min，max = 66.9 h。

**读法**

- doc 向量是**双峰**：约 53% 十分钟内拿到；但 **38.8% 要等 1–3 天**。
- chunk 向量**没有一个能在 1 分钟内拿到**，82% 等 ≥6 h，中位 13.4 h。

→ 这比"阶段串行几十分钟"严重一个数量级：**积压以天计**。

### 2.4 失败面

- `[INDEX] 提取失败` **366** 次；去重命中 5,569 次。
- PDF 解析告警巨量：`corrupt deflate stream` 1,511、`Could not parse the encoding, error: DictKey` 1,128、`ToUnicode CMap error` 数百、`failed to load PDF and no OCR fallback` 78、`OCR 回退也失败, 使用原始内容` 52。
- 这些是提取慢的一部分成因，也会反复重试（`fail_count` 上限 3）。

---

## 3. 瓶颈判定

| 段 | 单次规模 | 时间 | 瓶颈性质 |
|---|---|---|---|
| 扫描/提取 | 全量 ≈7,800 文件 | ≈2 h 墙钟 | 本地 CPU（PDF OCR） |
| doc 回填 | 全库 19.6k | 3.23 h（累计） | 远程网关 + 调度 |
| chunk 回填 | 全库 73.6k 块 | ≈6 h | **远程网关吞吐（硬上限）** |
| **文件等待向量** | — | **p75 = 58 h / chunk 中位 13.4 h** | **调度 + 单把大锁** |

**判定**：

1. **吞吐天花板** = 远程 `bge-m3` 网关 ≈2,300 tok/s。**流水线穿插无法突破它**，只能省掉"提取那一段"（整库重建约 2 h）。
2. **当前最严重的损失** = 回填调度与加锁：一次性快照 + 运行中丢弃调度 + 单锁被 chunk 长任务独占 → doc 回填被饿死 → 新文件以天为单位拿不到向量。
3. 因此"把各阶段穿插"是对的方向，但**优先级应是「持续回填 + 拆锁 + 新内容优先」，而不是「让提取和嵌入同一时刻开始」**。

---

## 4. 根因（按影响排序）

1. **单把 `BACKFILL_LOCK` + chunk 持锁到清空**（`embeddings.rs:17/312`）
   chunk 整库补齐（本次 6 h）期间，doc 回填线程在 `backfill_guard()` 上阻塞；而 `schedule_backfill_embeddings` 的守卫只判 `task == "backfill"`，**拦不住 `backfill_chunks`**，于是每次调度都会 spawn 一个新线程卡在锁上（`TaskGuard` 还没注册，守卫永远看不到）→ 阻塞线程堆积。
   证据：10-12 扫描结束（`04:11:17`）后启动的 chunk 回填从 `04:13:00` 持续到约 `10:07`；期间 `09:46`/`09:49` watcher 刚索引完文件，**之后没有任何 doc 回填启动**。

2. **一次性快照 + 运行中丢弃调度**（`embeddings.rs:23/190`）
   长扫描期间，每轮回填只能覆盖"取快照那一刻"已入库的文件；快照之后入队的行要等下一次 flush 或扫描结束。

3. **chunk 回填不参与扫描期**（`lib.rs:566`、`commands/index.rs:259`）
   `schedule_backfill_embeddings` 只触发 doc；chunk 只在启动/扫描完成后启动，所以"扫描期的空闲"里 chunk 一条都不补。

4. **无优先级**（`embeddings.rs:214`）
   历史 backlog 与刚索引的新文件同等对待（实际按 SQL 无序扫描），新内容无法插队。

5. （次要）**提取本身被少量 pathological PDF 主导**：1,729 个 >10s 的 PDF 占 55.9 h 线程时间，其中几个单文件 40–80 分钟。

---

## 5. 观测缺口（要"更全面"就得补）

| # | 缺口 | 影响 |
|---|---|---|
| 1 | 无队列深度 / 等待时长日志 | 不能用日志直接测"提取→向量"的延迟（本次靠 DB 时间戳补齐） |
| 2 | 无资源指标（CPU/IO/网卡/网关延迟） | "网关是天花板"是**推断**（依据：速率对 batch/并发不敏感） |
| 3 | 38.6% 日志行无日期前缀 | 无法放入时间轴 |
| 4 | 无 chunk 的 token 数、无 `run_backfill_chunks` 构建日志（一条都没有） | 无法判断 51,299 块是"首次全量"还是"重复重建"，也无法评估 1,500 字块长是否最优 |
| 5 | 提取耗时只在"开始+提取文字"两条都在时可得（9,538 / 15,349） | 去重命中与失败没有耗时 |

**建议补的 3 行观测日志**（纯观测，不改流程）：

```
[QUEUE] pending_doc=N pending_chunk=M            # 每轮回填打一次
[AI] 回填分片: n 块, 用时 Xs                      # 分片粒度，用于验证拆锁效果
[CHUNK] built N chunks for M docs                # 确认块数来源
```

---

## 6. 方案对比（建议顺序）

| 方案 | 做法 | 预期收益 | 风险 / 成本 |
|---|---|---|---|
| **① 拆锁 + 分片**（✅ 2026-10-12 已实施） | chunk 回填改为**每轮 pass 持有** `BACKFILL_LOCK`（`MAX_CHUNKS_PER_PASS` 4096→512 ≈ 3.6 min/轮）；doc 不再被饿死 | 修掉"新文件等 1–3 天拿到 doc 向量" | 小 |
| **② 常驻 drain worker** | 单个后台 worker：`doc 小批 → chunk 小批` 交替，扫描开始即启动，drain 到空且扫描结束才退出（替换 3s 防抖一次性快照） | 提取与嵌入真正并行；新内容 doc 向量从"天"→"分钟" | 中；要处理与手动回填命令/`vector_health` 的互斥 |
| **③ 新内容优先** | `missing_*` 查询 `ORDER BY updated_at DESC` | 刚索引的文件优先出向量 | 小 |
| **④ 修调度漏判**（✅ 2026-10-12 已实施） | `schedule_backfill_embeddings` 的 `PENDING` 覆盖「防抖 + 等锁 + 运行」整段窗口（原守卫只判 `== "backfill"`，拦不住 `backfill_chunks` → 线程堆积） | 阻止阻塞线程堆积 | 小 |
| ⑤ 减小 chunk 数 | `CHUNK_SIZE 1,500 → 2,500~3,000`（块数约 −40%） | 直接把 6 h 压到 ~3.5 h | **需先做检索质量对比（尚未评估）** |
| ⑥ 提升网关吞吐 | 多实例 / 换更强的嵌入服务 | 直接抬高 2,300 tok/s 上限 | 需基础设施投入 |

> **实施记录（2026-10-12，仅 ①+④）**：`src-tauri/src/commands/index/embeddings.rs`
> - 锁改为**分片持有**（chunk 每轮 pass 获取/释放），`MAX_CHUNKS_PER_PASS` 4096 → 512；
> - 新增 `DOC_BACKFILL_RUNNING` / `CHUNK_BACKFILL_RUNNING` 单飞标志（RAII `RunningFlag`），防手动命令与调度器并发跑同一份快照；
> - 调度 `PENDING` 覆盖整段窗口；每轮 pass 打 `[AI] chunk 回填分片: …`。
> - 效果：**doc 回填最迟 ~4 分钟内可插队**（原为整段 6 h 持锁）；②③ 未做。

**组合预期**（①②③④）：

- 新内容 doc 向量：p90 从"数天"→ 目标 10 分钟内。
- 整库重建墙钟：提取 ~2 h + chunk ~6 h 串行 ≈ 8 h → 重叠后 ≈ **max(2 h, 6 h) ≈ 6 h**（约 −25%）。
- 单次增量扫描：省下的是提取窗口（几分钟~几十分钟），且新内容立即可用。
- **那 6 h 的网关上限本身不会消失**，只有 ⑤/⑥ 能真正缩短。

---

## 7. 风险

- **SQLite 写竞争**：提取（8 并发）与回填同时写 `content_index` / `doc_chunks` / `doc_embeddings` / `chunk_embeddings`。当前 WAL + `busy_timeout=5000` + pool 24，需观察是否出现 `retry` 告警。
- **网关并发压力**：doc 与 chunk 同时打同一网关，总吞吐不变（共享上限），要限制在途请求总量，避免互相拖慢或触发限流。
- **本地模型场景**：若切回 `local:*` 嵌入，doc/chunk 并发会与本机 OCR 抢 CPU（这正是当年串行的原因），应保留"本地模型时串行/降并发"的开关。
- **重复/孤儿数据**：见附录 A。

---

## 附录 A：删除目录的两条路径（本次不改）

前端有**两个**删除入口，清理范围不同：

| 入口 | 命令 | Tantivy 文档 | file_tracking | content_index / doc_chunks | doc_embeddings | chunk_embeddings | doc_summaries / index_errors |
|---|---|:--:|:--:|:--:|:--:|:--:|:--:|
| 资料库页「删除资料库」 | `remove_dir`（`commands/dirs.rs:167`） | ✅ 逐文件 | ✅ | ✅（`cleanup_orphan_content`） | ✅（按 file_id） | ✅（**按 md5**） | ❌ 孤儿残留 |
| 备份/迁移页「清理失效目录」 | `remove_dir_with_files`（`commands/backup.rs:781`） | ❌ **不删** | ✅ | ❌ | ❌ | ❌ | ❌ |

要点：

1. `remove_dir` 逐个文件调用 `indexer.delete_file`（`indexer.rs:695`）：删 Tantivy 文档 → `mark_deleted` → 删 `doc_embeddings`（file_id）→ 删 `chunk_embeddings`（**md5**）；随后 `DELETE FROM file_tracking WHERE dir_id` 与 `cleanup_orphan_content`（清 `content_index`、`doc_chunks`），最后 `commit()`。
   - 副作用：按 md5 删块向量会**波及其他目录里内容相同（同 md5）的文件**（其块向量会在下次回填时重建）。
   - `doc_summaries`（file_id 键）与 `index_errors`（file_id 键）**不清理** → 孤儿行，仅占空间（AI 摘要走 `LEFT JOIN doc_summaries ON ds.file_id = f.id`，孤儿不参与）。
2. `remove_dir_with_files` 只删 `file_tracking` + `dir_config`，**不删 Tantivy 文档、不清理任何内容/向量表** → 会产生"幽灵"（见附录 C）。
3. `doc_embeddings` / `chunk_embeddings` 在第二条路径下无清理入口，长期只增不减。

## 附录 C：什么是"幽灵命中"

**定义**：Tantivy 索引里存在某文档，但 DB 中没有对应的活跃 `file_tracking` 行（或该行已 `status='deleted'`）——此时搜索结果里**仍会出现这个文件**，点开可能报错或文件已不在。

**成因（两条检索路径行为不同）**：

- **全文（BM25）**：结果直接由 Tantivy 文档构造（`search/searcher.rs:332`），**不回查 DB** → 没有 DB 行也会被返回。
- **语义（向量）**：候选在回填结果前会用 `get_file_by_id` 校验（`commands/search.rs:358`，取不到就跳过）→ 幽灵被过滤。

**触发条件**：`remove_dir_with_files`（备份页"清理失效目录"）只删 `file_tracking`，Tantivy 文档保留 → 该目录下的文件在**全文搜索**中持续命中，直到重建索引。

**与"重加"叠加**：该路径删后再重加同一路径，新 `file_id` 会再写一份 Tantivy 文档，旧的幽灵文档仍在 → **同一文件在全文搜索里可能出现两条**。

> 结论：日常"删除资料库"（`remove_dir`）不会产生幽灵；只有"清理失效目录"（`remove_dir_with_files`）会。修复方式自然是让它也走 `delete_file`/`delete_dir` + `cleanup_orphan_content`（另开任务）。

## 附录 D：md5 与"重跑"的语义（做对照实验前必读）

**md5 只由文件的原始字节决定**（`indexer.rs::extract_and_index_single`；>100 MB 的文件只取头 1 MB + 尾 1 MB 参与计算），与路径、所属目录、`data_dir` **无关**：

| 操作 | md5 值 |
|---|---|
| 删除目录再重加同一批文件 | **完全相同** |
| 复制文件到别的目录 / 别的磁盘 | **完全相同** |
| 修改文件内容 | 才会变 |

因此，**决定"要不要重新提取"的不是 md5 新不新，而是当前 `data_dir` 的 `content_index` 里有没有这个 md5 的行**：

- **删除再重加（同一 `data_dir`）**：`remove_dir` 删掉了该目录的 `content_index` 行 → **会重新提取**。唯一例外是**被其它目录共享的 md5**——`cleanup_orphan_content` 用的是 `DELETE … WHERE md5 NOT IN (SELECT DISTINCT md5 FROM file_tracking …)`，共享的会被保留 → 这些文件命中缓存、**跳过提取（不 OCR）**。（本库实测共享 28 个内容，占 0.4%。）
- **独立 `LINK_SEARCHER_DATA_DIR` + 复制子集**：新库 `content_index` 为空 → **全部重新提取**（md5 值与原库相同，但没有缓存行）。

**要点**：所谓"重制后全新"指的是**去重缓存为空**，不是 md5 变化；OCR 是否稳定也不影响 md5（md5 在读原始字节时算出，早于提取），只影响 `content_index.text`——只要缓存行还在就直接复用旧文本、不重跑 OCR。

## 附录 B：复现查询

```sql
-- 库现状
SELECT (SELECT COUNT(*) FROM file_tracking WHERE indexed=1 AND status='active'),
       (SELECT COUNT(*) FROM content_index),
       (SELECT COUNT(*) FROM doc_chunks),
       (SELECT COUNT(*) FROM chunk_embeddings),
       (SELECT COUNT(*) FROM doc_embeddings);

-- doc 向量延迟（秒）
SELECT ft.updated_at, e.updated_at
FROM file_tracking ft JOIN doc_embeddings e ON e.file_id = ft.id
WHERE ft.indexed=1 AND ft.status='active' AND e.updated_at >= ft.updated_at;

-- chunk 向量延迟（秒）
SELECT ci.indexed_at, MAX(ce.updated_at)
FROM content_index ci
JOIN doc_chunks dc ON dc.md5 = ci.md5
JOIN chunk_embeddings ce ON ce.md5 = ci.md5
GROUP BY ci.md5;

-- 仍在等待
SELECT COUNT(*) FROM file_tracking ft JOIN content_index ci ON ft.md5 = ci.md5
LEFT JOIN doc_embeddings e ON e.file_id = ft.id
WHERE ft.indexed=1 AND ft.status='active' AND e.file_id IS NULL;

SELECT COUNT(*) FROM doc_chunks dc
LEFT JOIN chunk_embeddings ce ON ce.md5 = dc.md5 AND ce.chunk_index = dc.chunk_index
WHERE ce.md5 IS NULL;
```

日志侧（PowerShell，注意 UTF-8）：

```powershell
Select-String -Path D:\index\app.log -Pattern '\[PERF\] 批索引总计' -Encoding UTF8
Select-String -Path D:\index\app.log -Pattern '\[AI\] (向量回填开始|chunk 向量回填开始|回填进度|回填完成)' -Encoding UTF8
```
