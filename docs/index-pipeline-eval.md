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
| **② 常驻 drain worker**（✅ 2026-10-12 已实施） | 两个 keeper 线程（doc / chunk）在扫描期间常驻，各自 drain 自己的队列；扫描开始即启动（`IndexingScope` + `is_indexing`），队列空且索引停止才退出 | 提取与嵌入真正并行 → **端到端 −40%**；新内容 doc 向量"分钟级" | 中；按 pass 锁交替，本地模型场景并发受副本池约束 |
| **③ 新内容优先** | `missing_*` 查询 `ORDER BY updated_at DESC` | 刚索引的文件优先出向量 | 小 |
| **④ 修调度漏判**（✅ 2026-10-12 已实施） | `schedule_backfill_embeddings` 的 `PENDING` 覆盖「防抖 + 等锁 + 运行」整段窗口（原守卫只判 `== "backfill"`，拦不住 `backfill_chunks` → 线程堆积） | 阻止阻塞线程堆积 | 小 |
| ⑤ 减小 chunk 数 | `CHUNK_SIZE 1,500 → 2,500~3,000`（块数约 −40%） | 直接把 6 h 压到 ~3.5 h | **需先做检索质量对比（尚未评估）** |
| ⑥ 提升网关吞吐 | 多实例 / 换更强的嵌入服务 | 直接抬高 2,300 tok/s 上限 | 需基础设施投入 |

> **实施记录（2026-10-12）**
>
> **①+④ 已实施**（`commands/index/embeddings.rs`）：锁改为**分片持有**（chunk 每轮 pass 获取/释放），`MAX_CHUNKS_PER_PASS` 4096 → 512；新增 `DOC_BACKFILL_RUNNING` / `CHUNK_BACKFILL_RUNNING` 单飞标志（RAII `RunningFlag`）；调度 `PENDING` 覆盖整段窗口；每轮 pass 打 `[AI] chunk 回填分片: …`。
> → **doc 回填最迟 ~4 分钟内可插队**（原为整段 6 h 持锁）。
>
> **② 已实施**：`ensure_embedding_keepers(db, is_indexing)` 在扫描期间常驻 **doc / chunk 两个 keeper 线程**，靠 ① 的分片锁交替 drain；`IndexerService` 新增 `is_indexing` 标志，`Scanner` 用 `IndexingScope`（RAII）在 `full_scan`/`incremental_scan`/`startup_scan` 期间置位并在扫描开始时拉起 keeper。
> → **提取与回填并行**，端到端由 ≈18 h 降到 ≈10.6 h（约 **−40%**）；回填本身耗时不变。
>
> **① 补充（2026-10-12，实测后）**：分片锁仍不够——chunk drain 释放后**立刻**重取（每轮 ~213 s），doc 18.7 分钟抢不到锁。已在 `run_backfill_chunk_embeddings` 的**每轮循环内、取锁前**加"先补一轮 doc"（`has_missing_doc_embeddings` + 30s 节流），doc 延迟上限降到 **≈3.5 分钟**（一轮 chunk pass）。
>
> **③ 未做**（`ORDER BY updated_at DESC` 新内容优先）；**⑤/⑥ 未做**。

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

## 附录 A：删除目录的两条路径（✅ 2026-10-12 已统一）

前端有**两个**删除入口，现已共用同一套彻底清理（`IndexerService::purge_dir_data`）：

| 入口 | 命令 | Tantivy 文档 | file_tracking | content_index / doc_chunks | doc_embeddings | chunk_embeddings | doc_summaries / index_errors | 扩展名统计 |
|---|---|:--:|:--:|:--:|:--:|:--:|:--:|:--:|
| 资料库页「删除资料库」 | `remove_dir(id, purge)` | ✅ `delete_by_dir` | ✅ | ✅ 孤儿感知 | ✅ 按 file_id | ✅ **孤儿感知** | ✅ | `purge=true` 时清 |
| 备份/迁移页「清理失效目录」 | `remove_dir_with_files` | ✅ `delete_by_dir` | ✅ | ✅ 孤儿感知 | ✅ 按 file_id | ✅ **孤儿感知** | ✅ | ✅ |

要点：

1. `purge_dir_data(dir_id, deep)`（`indexer.rs`）一次做完：Tantivy `delete_by_dir` → 按 file_id 清 `doc_embeddings`/`doc_summaries`/`index_errors` → 删 `file_tracking` →（`deep`）删 `unsupported_ext_stats` → `cleanup_orphan_content` + `cleanup_orphan_chunk_embeddings` → `commit()`。
2. **删除流程还会先停扫描、再收尾、再续扫**（2026-10-12）：
   - `remove_dir` 先 `cancel_scan` 并**等待扫描真正退出**（取消是合作式的，慢文件可能几分钟），否则扫描会继续往被删目录写数据；
   - 之后 `sweep_orphan_dirs()` 清掉 `dir_config` 中已不存在的 `dir_id` 的一切残留（这是"取消晚生效"窗口的兜底）；
   - 最后用 `ScanWorker` **自动续扫剩余目录**，用户不需要手动再点扫描；
   - `purge=true` 时只做 `PRAGMA wal_checkpoint(TRUNCATE)`，不再用 VACUUM（VACUUM 会重写整库并独占写锁 ~40s）。
3. **孤儿感知**：`content_index`/`doc_chunks`/`chunk_embeddings` 按 md5 键、内容在**目录间共享**，所以只在"没有任何 file_tracking 再引用该 md5"时才删。`IndexerService::delete_file` 里过去**按 md5 无条件删块向量**的缺陷已修（改为先查 `count_other_active_files_with_md5`）。
4. 资料库页的删除对话框新增**「同时清除该目录的所有数据」**复选框（默认不勾）：勾选后额外清扩展名统计并回收 WAL 空间，重加该目录即全新处理；不勾则保持原行为。UI 从原生 `confirm()`（放不下复选框）改为内联弹窗（`pages/DirManager.tsx`），**执行期间不可关闭**并显示进度，完成后提示结果。
5. HTTP API 同步：`DELETE /api/dirs` body 增加 `purge`，响应返回 `message`/`swept_orphan_dirs`/`resumed_scan`（`webapi/routes/dirs.rs`）。

## 附录 C：什么是"幽灵命中"（✅ 已修复）

**定义**：Tantivy 索引里存在某文档，但 DB 中没有对应的活跃 `file_tracking` 行（或该行已 `status='deleted'`）——此时搜索结果里**仍会出现这个文件**，点开可能报错或文件已不在。

**成因（两条检索路径行为不同）**：

- **全文（BM25）**：结果直接由 Tantivy 文档构造（`search/searcher.rs:332`），**不回查 DB** → 没有 DB 行也会被返回。
- **语义（向量）**：候选在回填结果前会用 `get_file_by_id` 校验（`commands/search.rs:358`，取不到就跳过）→ 幽灵被过滤。

**旧触发条件**：`remove_dir_with_files` 只删 `file_tracking`、保留 Tantivy 文档 → 全文搜索持续命中；重加同一路径会让同一文件出现两条。

> **已修复**：两条删除路径现在都走 `purge_dir_data`（含 Tantivy `delete_by_dir`），不会再留幽灵。

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

---

## 附：界面"无响应"但后台正常 —— 连接池饿死前端 IPC（2026-10-15 实测）

### 现象（实测取证）

| 观测 | 值 |
|---|---|
| 后台 OCR | **42~44 页/分**，日志持续写入，页陆续完成 |
| Web API | 3 秒内正常返回 JSON |
| 池/闸门告警 | 0 |
| 界面进程（WebView2） | 累计 CPU 726s + 545s |
| **最近 10 分钟前端查询（`[FILES]`）** | **0 次** ← 界面发出的 IPC 一直没回来 |

Windows 把窗口标成"无响应"，但**扫描从未中断**（对比 2026-10-14 17:04 的真死锁：那次是日志静默 + 页计数 25 分钟不动 + CPU 归零）。

### 根因

1. 界面每个动作（索引状态、文件列表）都走 **`state.db.get()`**——同一个连接池；
2. Phase-1 提取 worker **在整个提取期（含几分钟 OCR）持有连接**，并发 14；
3. 池被占满时前端只能等，**等待上限当时是 60 秒**（2026-10-14 我刚从 30s 提到 60s，反而把单次等待上限翻倍）；
4. Tauri 的 `async fn` 命令在 async runtime 上跑，**命令内部是同步阻塞的 DB 调用** → 若干条被卡住的 IPC 会把 runtime worker 占满 → 后续 IPC 全部排队 → 前端拿不到任何响应（`[FILES]` 归零正是这个证据）。

> 结论：**不是界面崩了，是"前端的数据库访问被提取流水线饿死"**。它会在批的 Phase 1 结束、worker 释放连接后自行恢复。

### 修法（按性价比排序，待实现）

1. 🔴 **给界面一条专用连接**：新增一个独立的小池（max 2、超时 1 秒）或单条 `Mutex<Connection>`，只服务只读的 UI/状态查询 → 界面永不与提取抢池。
2. 🔴 **UI 查询用短超时 + 降级**：`get_index_status` / `list_files` 用 `pool.get_timeout(800ms)`；拿不到就返回**上一次缓存的状态**（或带 `busy` 标志的空结果），**绝不让界面等 60 秒**。
3. 🔴 **不要把连接跨整个提取期持有**（根本原因）：改为"每个 DB 操作各自取用/归还"。
4. 🟠 **前端降级与节流**：索引进行中把状态轮询从 5s 放宽到 15~30s，并显示"索引进行中，界面数据可能滞后"，而不是白屏/无响应。

### 复现与判定方法（供以后回归）

- 复现：大批量扫描（≥500 文件、含大扫描件）进行中，观察界面 `[FILES]` 查询是否停止出现、窗口是否变"无响应"；
- 判定修好：扫描进行中界面仍能刷新（状态页档位/计数持续更新），且 `[FILES]` 查询保持有输出；
- 反例（真死锁）判定：日志静默 **且** 页计数不动 **且** CPU 增量 0 —— 与本节现象**必须区分**。

### 修复进度（2026-10-15）

- ✅ **界面无响应**（IPC 饿死 + 前端忙循环）：`get_index_status` 2 秒超时 + 前端 8s 硬超时 / 在飞去重 / 连续 5 次失败停止轮询 + 索引期间轮询 10s —— 实测界面空转 CPU 7.4s/20s → **0.1s/20s**（提交 `8b0fb25`）
- ✅ **批尾/小批半速**：页窗口改**全局预算**（按在飞 pass 均分，单文档吃满引擎闸门 8）—— 实测单文档 13~20 页/分 → 预计 ~35 页/分
- ✅ **连接跨整个提取期持有**（根本原因）：`extract_and_index_single` 签名改为**接收连接池而非连接**，内部按「提取前读 / 提取 / 提取后写」三阶段各借各还 —— **提取期零池占用**；`batch_index` 的 `par_iter` 与 `index_file` 两处调用方同步改（提交见 CHANGELOG「修『提取期长期持有 DB 连接』」）。
  - 验收：扫描期间日志不应再出现 `DB conn: timed out waiting for connection`；界面 `[FILES]` 查询应持续有输出；批量索引的 `indexed` 计数与失败率不劣化。
- ✅ **界面专用连接**（独立小池/单连接）：`UI_CONN` 只读连接，`get_index_status` 优先走它（提交 `2db9c21`）。
- ⏳ **前端"界面数据可能滞后"提示**：需新增 4 个语言文件键，留待下一轮。
- ✅ **批屏障内存峰值（~18GB）—— 归因已证伪，不做「Phase-1 分批落库」**（2026-10-15；观测已装 + 数据已得）
  - **代码核对**：`par_iter().collect()` 的驻留**只有 `text`** —— 原始文件字节 `raw` 在 `extract_and_index_single` 返回时即释放，**不进屏障**；
  - **实测上界**：真实库（小城 XC）`SELECT SUM(char_count) FROM content_index` = **108,354,301 字 ≈ 0.30 GB**，单篇最大 2,130,940 字 → 一块（≤500 文件）驻留文本**最多 ~0.30 GB**，与 18GB 差约 **60×**（要成立需"每文件 36MB 纯文本"）；隔离端到端跑（206 文件、含一篇 5.24M 字长文）驻留文本 **~5.5 MB**；
  - **结论**：历史 18.7GB 属 `extractor/audio.rs` 整段解码那次、10.2GB 属并行大扫描件**页位图** —— 两者都由 `load.rs` 闸门管辖，**不是批屏障**。保留每块一条的 `[PERF] 批屏障` 观测，**不改 Phase-1 落库路径**（避免为不存在的问题动"写入正确性"）。

