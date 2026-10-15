# Link-Searcher Agent 规范

> 每次进行代码变更时自动读取，确保一致性。

## 完整工作流（每次代码变更强制执行）

```
用户提需求
  ↓
1. 理解 & 复述：Agent 用自己的话复述需求，确认理解正确
  ↓ （如歧义→反问澄清；如简单→跳到4）
2. 方案提案：给出具体修改方案（改哪些文件、怎么改、影响面）
  ↓
3. 用户确认：必须等用户说"好的/开始/确认"后才动手
   ← 禁止在用户确认前开始写代码 ←
   ← 最容易跳过的一步：日志分析完/根因找到后 → 先问"要修吗？"再动手，不要直接编辑 ←
  ↓
4. 实施 & 测试：按确认的方案修改代码，写测试/跑现有测试
  ↓
5. 文档 & 审计：
   a. 更新 CHANGELOG.md（每次 commit 必更新）
   b. 更新 README.md（涉及功能/架构变化）
   c. 更新 USER_MANUAL.md（涉及用户可见行为变化）
   d. semgrep scan --severity ERROR 零发现
   e. cargo check / npx tsc 零错误
   f. **跑全部测试目标**（不只是 `--lib`）零失败 —— 见「测试门禁」
  ↓
6. 提交 & 推送：git commit + git push（CHANGELOG 必须同在此 commit）
```

> **例外**：纯修 typo、单行 config 改值、仅文档修改等**微不足道**的变更，可跳过步骤 2–3，直接实施。

## 测试门禁（提交前必跑全部测试目标）

**禁止**把 `cargo test --lib` 当作提交门禁：它只覆盖单元测试，`src-tauri/tests/` 下的集成测试与
mock E2E **完全不参与编译与运行** —— 测试目标一旦编译不过或断言过时，就会长期无人发现。

**为什么立这条规矩（2026-10-15 实测的三连击）**：

| 提交 | 时间 | 后果 | 潜伏期 |
|---|---|---|---|
| `ada8485` | 2026-10-12 | 给 `remove_dir` 加 `app: AppHandle` → `auto_ui_e2e` **编译不过** | **3 天**（整个目标等于没在跑） |
| `2db9c21` | 2026-10-15 | 引入 `UI_CONN`（界面专用连接）→ 静默打破 `ipc_test::test_ipc_get_index_status` | 当日发现 |
| — | — | 两者都只跑过 `--lib`，故都未被察觉 | — |

### 必跑命令（全部通过才可提交）

```bash
# 全部测试目标（应用 15 个目标 + 2 个成员 crate）；约 3 分钟，其中编译占大头
#
# ⚠️ 必须带 `--workspace`：`src-tauri` 既是 workspace 根**又是** package，此时
#    `cargo test` 默认**只作用于主程序包** → 成员 crate `lark-gate` 会静默不跑，
#    而那段准入判定是"仅私聊 + 白名单"的**唯一防线**（2026-10-15 拆 crate 时立的规矩）。
# ⚠️ 换机器/全新克隆后先跑一次 `npm run sidecar`：`bundle.externalBin` 在**编译期**
#    就要求 `src-tauri/binaries/lark-bot-<triple>.exe` 存在，缺了 `cargo test` 会以
#    `failed to run custom build command` 失败（看起来像代码坏了，其实是缺文件）。
cargo test --manifest-path src-tauri/Cargo.toml --workspace

# 前端
npx tsc --noEmit
npx vitest run
```

> 不要用 `--lib`、`--test <单个>` 或 filter 缩小范围作为提交门禁；只在**定位单个失败**时才缩小。

### 目标清单与实测耗时（本机基线）

| 目标 | 用例 | 耗时 | 性质 |
|------|:----:|:----:|------|
| `--lib` | 557 | 7.2s | 单元测试（拆分后：566 − `lark-gate` 的 9 个搬到成员 crate） |
| `lark-gate`（成员 crate） | 9 | 0.00s | **准入判定策略**（仅私聊 + 白名单，唯一防线；`-p lark-gate` 单独跑） |
| `lark-bot`（成员 crate） | 3 | 0.00s | bot 纯函数（回复截断等；`-p lark-bot`） |
| `ai_chat_stream_wire` | 15 | 39.7s | mock SSE 网关（**最慢**，但无网络依赖） |
| `auto_ui_e2e` | 15 | 0.5s | mock runtime（`MockRuntime`） |
| `ipc_test` | 8 | 5.3s | mock runtime |
| `integration` | 9 | 1.8s | 真实模块（db/indexer/scanner/search） |
| `webapi_http` | 冒烟 | ~0s | 真 axum + 真 Wry app（`harness = false`，主线程） |
| `focus_mode_test` / `merge_scope_test` / `test_pagination` | 4 / 4 / 2 | <0.1s | 纯逻辑 |
| `doc_rs_poc` / `doc_rs_poc_fallback` | 1 + 1 | 0.00s | POC 证据 |
| `test_pdf_inspector_ab` | 2（+1 ignored） | 0.02s | A/B 对比 |
| `funasr_smoke` | 3 | 0.00s | **缺 env/模型时防御性早退** |
| `ocr_eval` | 1 | 0.23s | **缺 fixtures 时早退** |
| `test_pdf_ocr` | 2 | 0.00s | **缺样例时早退**（含硬编码 macOS 路径） |
| `remote_embed_adaptive_bench` | 0（1 ignored） | 0.00s | 手动基准，需 `--ignored` + 网络 |

**注意**：后 4 项在缺少样例/模型/env 时**快速早退并通过**，属正常；但一旦样例存在它们会真跑
（可能很慢/需要模型），因此在**换机器或放入样例后**要留意耗时变化。`#[ignore]` 的用例默认不跑，
需显式 `-- --ignored`。

### ⚠️ 门禁命令的两个坑（2026-10-15 实测踩到）

**① 不要用 `Select-Object -First N` 截断 cargo 的输出**

PowerShell 在取够 N 行后会**提前关闭管道**，把 cargo 进程**半途掐断**，留下**写了一半的元数据**。
下一次构建就会以看似严重的错误失败：

```
error[E0786]: found invalid metadata files for crate `link_searcher_lib`
```

**它看起来像代码坏了，其实是缓存坏了** —— 重跑一次就好。截断输出请改用 `Select-String` /
`Where-Object`（会消费完整个流），或先把输出收进变量再筛。

> 若真见到 `E0786`：**先重跑一次**再下结论；仍然失败再 `cargo clean -p link-searcher`。

**② 门禁必须"失败即停"**

**不要**把 `cargo test` 和 `git commit` 串在同一条命令里无条件往下跑 —— 测试红了它照样会提交。
正确做法：先把门禁跑绿，**确认之后再单独提交**（或在脚本里 `if ($LASTEXITCODE -ne 0) { exit 1 }`）。

**③ 不要用 PowerShell 的 `Set-Content` / `-replace | Set-Content` 改源码**

PowerShell 5.1 的 `Set-Content` 默认按**系统 ANSI 编码**写文件（本机中文 Windows 上是 GBK）。
拿它改 UTF-8 源码会把**所有非 ASCII 字符写坏**（中文注释全变乱码），而编译器通常**不会报错**
—— 因为坏掉的是注释而不是字符串字面量，于是损坏**静默通过**。

→ 改源码**一律用编辑工具**（`edit` / `write`）。**绝不**用 `Set-Content` / `Out-File` / `-replace` 落盘。
（2026-10-15 实测：`-replace | Set-Content` 把 `webapi/routes/lark.rs` 的中文注释写成了乱码，只能整份重写。）

**即使显式带上 `-Encoding UTF8` 也不行**：PS 5.1 的 `-Encoding UTF8` 会写入 **BOM**（`EF BB BF`），
把源码首字节污染；不指定编码则按 ANSI 写、直接写坏中文。**两种都不对**。
（2026-10-15 再次实测：明知此条规则、仍用 `Set-Content -Encoding UTF8` 改了 `indexer.rs` —— 中文侥幸没坏，
但**加了 BOM**，只能用 .NET 再写一遍去掉：
`[IO.File]::WriteAllText($p, [IO.File]::ReadAllText($p), (New-Object System.Text.UTF8Encoding $false))`。
结论不变且更强：**改源码只用编辑工具**，一次也别例外。）

**读**源码同理：PS 5.1 的 `Get-Content` 默认也按 ANSI 解码，读 UTF-8 文件会**显示成乱码**（文件其实是好的）。
看到"乱码"先换 `Get-Content -Encoding UTF8`（或直接用编辑工具读）再判断 —— 别去"修"一个健康的文件。

**④ 测试不得依赖"全局负载档位"或机器环境**

`crate::load::*`（自适应并发闸门）与**系统内存压力**相关，取值会随机器状态变化。任何断言
`page_window` / 并发上限 / 耗时的测试都会因此**假红**，而且**不同时刻红的是不同那条** ——
极易被误判成"flaky"从而放过真正的缺陷。

→ 这类测试要**先读运行时实际值**（如 `load::page_window_for_pass()`），再据此推导 sleep 与断言上限，
**不要写死**与它耦合的常数。
（2026-10-15 实测：`windowed_pages_stall_stops_dispatch_but_keeps_results` 写死 `stall=10ms` / 每页
`sleep=50ms`，隐含假设 `window ≤ 4`；页窗口改为"全局预算、上限 8"后随负载漂移而红，且在 HEAD 上
**同样失败** —— 属既有缺陷，非当时改动引入。）

**④′ 测试把 `%TEMP%` 整个当"被索引目录"，或共用固定名临时目录（2026-10-15 实测的红法）**

有些测试把 `std::env::temp_dir()` **本身**注册成被索引目录（`dir_config::add_dir(&c, temp_dir, …)`），
或让一组测试共用固定名目录（如 `idx_test_<pid>`）。并行跑时**别的测试往那里写的文件会被卷进本次统计**
→ `healed == 1` / `db_indexed == 2` 这类断言随并行时序失败，而**单独跑必过**（"看着像 flaky"的典型）。

→ 这类测试必须**独占**一个目录。`commands/index.rs` 已加 `isolated_temp_root()`（每次调用都不同），
`integrity_heal_*` / `verify_*` 共 5 条已改（改前约 1/3 轮次红，改后连跑 4 轮全绿）。
**仍未改（已知同等风险模式）**：`scanner/mod.rs:871,1059` —— **但复读代码后判定它无害**：
每个测试都会在扫描前用 `UPDATE dir_config SET path=…` 把它换成**自己独占的子目录**，
`%TEMP%` 只是"注册时的占位值"，没有任何扫描会在它还是 `%TEMP%` 时发生。
（`indexer.rs::tmp_file` 的共享目录同理：它只当**文件容器**用，各测试用的文件名互不相同，也没有测试拿它的路径去 `add_dir`。）

**⚠️ 另一条"看着像同类、其实不是"的 flaky**：`indexer::tests::purge_dir_data_clears_dir_but_keeps_shared_content`
（实测 **6 轮红 1 次**；换 `--nocapture` 连跑 8 轮又全绿 —— 典型时序敏感）。

**已排除**"共享 %TEMP% 目录"这个解释：它用 uuid 建库（`idx_pool_<uuid>`）+ 进程内 RAM 索引，
断言全是**本测试自己的 DB 计数**；而且失败时 `file_tracking WHERE dir_id='d1'` **已经是 0**
（说明 `purge_dir_data` 确实跑到了），**只有 `doc_embeddings` 没被清掉**。

→ **更精确的诊断（第二轮排查，已读 `purge_dir_data` 本体）**：它是**同步**的，并且顺序正确 ——
**先**删 `doc_embeddings`（`file_id IN (SELECT id FROM file_tracking WHERE dir_id=?)`）**再**删 `file_tracking`。
但注意这条子查询**依赖 `fid` 的追踪行仍然存在**；而 `setup()` 给 `fid` 的路径是**磁盘上不存在**的
`/tmp/test.txt` → 只要后台 **keeper / 清理**恰好先把它当"死文件"清掉，purge 就会**漏掉** `fid` 的向量。
失败时"`file_tracking(dir_id='d1')` 已是 0、**只有** `doc_embeddings` 没清"**与该推测完全吻合**。

→ 顺带暴露一个**真实产品缺口**：以 `file_id` 为键的 `doc_embeddings` / `doc_summaries` / `index_errors`
在追踪行消失后**没有清道夫**（现有的 `cleanup_orphan_content` / `cleanup_orphan_chunk_embeddings` 是 **md5 键**的）。
`purge_dir_data` 之所以能清，全靠"删之前追踪行还在"这个前提。

→ **已修（选了产品侧 (b)，2026-10-15）**：`purge_dir_data` 原本三条按 `file_id` 的 DELETE **各自独立执行、失败只 warn**
—— 并发写入者（keeper / 周期提交）短暂持锁时它们会**静默失败**，而紧随其后的 `DELETE FROM file_tracking`
却可能成功 → 正好留下"追踪行没了、`doc_embeddings`/`doc_summaries` 还在"的孤儿（与实测症状完全吻合）。
现改为**同一事务 + 传播错误**（`unchecked_transaction`，顺序不变：先 file_id 行、再 file_tracking；
`unsupported_ext_stats` 同归事务），两步 md5 键的孤儿清理仍保持尽力而为。
**验证**：单条 **12/12** 绿、全 lib **再压 6/6** 绿（改前约 1/6 轮次红）。
> 教训：**"失败只 warn" 的 DELETE 就是"偶发脏数据"的温床** —— 多语句的清理要么同一事务，要么带重试。

**⚠️ 遗留（如实记录，不假装已解决）**：B 项落地期间有一次 `cargo test --workspace` **返回非 0**
（当时被日志过滤词掩盖了用例名）。修完 A 之后复扫 **15 轮全绿**（10× `--lib` + 5× 全 workspace）
→ **无法复现**；该时段恰逢运行环境重启（我的提交命令也被打断过）→ 最可能是**环境因素**而非代码。
→ 若再现：用"脚本化连跑 N 轮 + 失败即抓 `Running <target>` 标记与 `left/right`"定位，
**不要**靠重跑碰运气（重跑只会掩盖名字）。

> **再现的第二个签名（2026-10-16，D 第 2b 步门禁）**：`error: linking with link.exe failed: exit code 1327/1158`
> （多个目标同时报）—— 但**磁盘充足**（C 空 74GB / D 空 182GB），且**紧接着重跑 EXIT=0**。
> → 同属环境性抖动（疑似并发链接 / 杀软 / 僵尸测试进程持锁）。**先重跑一次再下结论**，
> 别急着改代码。顺带一提：`target/debug` 已 **51 GB**（AGENTS 里那条"曾达 21.9GB"的记录要更新认知）。

**⑤ 结构性改动有"门禁盲区"：必须真的起一次应用**

给应用**新增 bin 目标**、加 `default-run`、换入口点这类改动，`cargo build` / `cargo test` **都不会**
暴露问题 —— 它们不需要"跑哪个二进制"的推断。但 `tauri dev` 内部是用 `cargo run` 起应用的：

> 2026-10-15 实测：新增 `src/bin/lark-bot.rs` 后 `npx tauri dev` 直接失败
> （`could not determine which binary to run ... available binaries: lark-bot, link-searcher`），
> 而**同一提交的全门禁是绿的**。

→ 此类改动提交前**至少起一次应用**，或至少验 `cargo run <flags> -- --help` 能成功推断。

**⑤′ 同类盲区：`bundle.externalBin` 在**编译期**就要求 sidecar 文件已存在**

（2026-10-15 做 bot 打包时实测）`tauri-build` 在 `build.rs` 里校验 `externalBin`：**文件缺失即构建失败**
（`failed to run custom build command`），而 `cargo test` / `cargo build` 都会走 `build.rs`。
于是"生成那个文件"的脚本若反过来必须先编主程序 —— **鸡生蛋**，`tauri dev` 当场起不来。

现在 bot 是成员 crate `lark-bot`（**不在主程序包里**）：`npm run sidecar` 只跑
`cargo build -p lark-bot`，**完全不触碰主程序** → 环已断（`docs/LARK_BOT.md` §5.7）。

→ **换机器 / 全新克隆**后先跑一次 `npm run sidecar`，再 `cargo test`；
   `npm run dev` / `npm run tauri:build` 由 `predev` / `prebuild` 自动兜住（无需记得）。
→ 又一个"全门禁绿、真跑才炸"的例子：这类改动**必须真起一次应用**（见 ⑤）。

**⑤″ 构建与运行会互锁：门禁前先停应用（2026-10-15 实测两次）**

`tauri-build` 在编译期会把 sidecar **复制**到 `target/debug/lark-bot.exe`。而**运行中的 bot 进程锁着这个
目标文件** → 复制失败 → `cargo test` / `cargo build` 直接红：

```
thread 'main' panicked at tauri-build-2.6.3/src/lib.rs:80:
called `Result::unwrap()` on an `Err` value: Os { code: 5, kind: PermissionDenied, "拒绝访问。" }
```

以前只知道 `link-searcher.exe` 会占住应用二进制，**现在 bot 也会占住 sidecar** —— 两个都要停。
`tauri dev` 里点过自启/开着窗口时同样会中招。

→ **门禁前先停掉正在跑的应用**（`Stop-Process -Name link-searcher`；bot 会随看门狗自退）。

⚠️ **但只停应用还不够**：`tauri dev` 在**监听源码**，一改文件它就 `Rebuilding application...` 并**重新起一个**应用
（2026-10-15 实测：改完 `webapi/session.rs` 后跑门禁，又撞上 `failed to remove file …link-searcher.exe`）。
→ 要跑门禁就**把 `tauri dev` 本身停掉**（杀掉命令行含 `tauri` 的 `node` 进程），而不是只杀应用。

```powershell
Get-CimInstance Win32_Process -Filter "Name='node.exe'" |
  Where-Object { $_.CommandLine -match 'tauri' } |
  ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
Stop-Process -Name link-searcher -Force -ErrorAction SilentlyContinue
```

**⑤‴ bot 改了源码，`tauri dev` 不会重建 sidecar**

bot 是**成员 crate**，不是应用包的依赖 → `tauri dev` 的文件监听**只重建应用包**。
改了 `src-tauri/lark-bot/**` 后**必须重启 dev**（`predev` 才会重编 sidecar），否则跑的还是旧二进制
（2026-10-15 实测：第一次飞书验收跑的就是旧 bin，白测一轮）。

## 静态分析（Semgrep）

每次提交前，必须运行阻塞级检查，**零发现**才可提交：

```bash
semgrep scan \
  --config .semgrep/custom.yml \
  --config p/owasp-top-ten \
  --config p/secrets \
  --severity ERROR
```

> **Windows 运行注意**（本机已装 semgrep 1.177 / Python 3.12，`semgrep.exe` 在
> `%LOCALAPPDATA%\Programs\Python\Python312\Scripts`）：
> - Python 在 Windows 默认用 GBK 读文件，会因 `.semgrep/custom.yml` 含 UTF-8 中文而崩溃 → 必须先设 `$env:PYTHONUTF8='1'`（已写入用户环境变量，新终端生效）。
> - 若 `semgrep` 不在 PATH：`$env:Path += ";$env:LOCALAPPDATA\Programs\Python\Python312\Scripts"`。

| 级别 | 含义 | 包含规则 |
|:---:|------|------|
| **ERROR** | 🔴 阻塞提交，必须修复 | OWASP Top 10、密钥泄露、RwLock/Mutex 锁中毒 |
| WARNING | 🟡 不阻塞，需定期 review | p/rust、p/typescript、p/react、unwrap/expect、fs::copy |
| INFO | ⚫ 仅记录，参考用 | let_ 静默丢弃、ok() 吞错误 |

完整扫描（含 WARNING 级）：
```bash
semgrep scan \
  --config .semgrep/custom.yml \
  --config p/rust \
  --config p/typescript \
  --config p/react \
  --config p/owasp-top-ten \
  --config p/secrets
```

⚠️ 子任务**不应修改**此节或自行追加 semgrep 规则。规则变更由主 Agent 统一管理。

## 变更记录（CHANGELOG.md）

**每次修改代码后，必须同步更新 `CHANGELOG.md`**，再一起提交。

条目格式：
```markdown
- **简要描述**：根因是什么、怎么修的、涉及哪些文件
```

示例：
```markdown
- **删除文件无反应**：`mark_deleted` SQL `WHERE path=?` 错误接收 UUID（file_id），改为 `WHERE id=?`（`tracker.rs`）
```

不要只在 commit message 里写一行标题就完事。

**注意**：多个并行子任务修改代码时，由主 Agent 统一整理 CHANGELOG 顺序，禁止子任务各自乱追加导致轮次交错。

## 代码规范

- Rust edition 2024，禁止 `unwrap()` / `expect()` 在非致命路径
- TypeScript strict 模式，禁止 `any`
- 前端组件文件命名 PascalCase，hooks 用 camelCase
- IPC 命令用 Tauri `#[tauri::command]`，`async fn` 避免阻塞事件循环
- 新增命令必须在 `lib.rs` 的 `invoke_handler` 中注册

## 项目关键文件

| 文件 | 说明 |
|------|------|
| `src-tauri/src/lib.rs` | Tauri 初始化 + 启动流程 + 命令注册 |
| `src-tauri/src/scanner/mod.rs` | 全量/增量/启动扫描 |
| `src-tauri/src/indexer.rs` | 索引服务（batch_index / MD5 / 去重） |
| `src-tauri/src/db/tracker.rs` | 文件追踪 CRUD + 统计（子模块 `db/tracker/{content,embeddings}.rs`） |
| `src-tauri/src/extractor/paddleocr.rs` | PaddleOCR 内置引擎 |
| `src-tauri/src/extractor/pdf.rs` | PDF 提取入口（子模块 `extractor/pdf/{poppler,scan,quality,ocr}.rs`） |
| `src-tauri/src/commands/ai.rs` | AI 摘要 / RAG 问答 / 多轮聊天（子模块 `commands/ai/{cite,session,rewrite,retrieval,prompt}.rs`） |
| `src-tauri/src/commands/index.rs` | 索引状态/扫描/重建（子模块 `commands/index/{embeddings,verify,integrity}.rs`） |
| `src/pages/SearchPage.tsx` | 搜索页 |
| `src/pages/Browse.tsx` | 浏览页（表格视图） |
| `src/pages/IndexStatus.tsx` | 索引状态页 |
| `src/pages/Settings.tsx` | 设置页 |
| `CHANGELOG.md` | 变更日志（每次 commit 必更新） |

## ⚠️ 反复出现的问题清单（修复后禁止再次出现）

### 1. Phase 1 索引进度不可见（indexed=3 被丢弃）
- **现象**：批量索引时，Phase 1 提取完成但 UI 显示已索引=0
- **根因**：`mark_extracted` 用 `let _` 吞错误，SQLite 并行写冲突静默失败
- **修复**：`indexer.rs` 中 `mark_extracted` 失败时打 WARN 日志 + 重试一次
- **检查方法**：批量索引时看 DB 中 `SELECT indexed, COUNT(*) FROM file_tracking GROUP BY indexed` 是否有 indexed=3
- **触发条件**：Rayon par_iter + SQLite 并行写
- **修复人/时间**：2026-08-08
- **Tags**: `indexer.rs`, `mark_extracted`, `Phase 1`, `let _`

### 2. AI 聊天无回复（max_tokens 超限 → HTTP 400）
- **现象**：发问后界面无回答；日志 `chat stream failed: ... status code 400` + `chat_stream returned: chars=0`
- **根因**：把猜测的 `max_tokens`（未知时回退 1,000,000）发给校验型网关（agnes 上限 65,536）被 400 拒绝；且**修复只落在非流式路径**，`chat_stream()` 没有降级。**聚合网关**同一 id 每次路由不同上游，上限不稳定，"缓存一个值"也不可靠
- **修复**（2026-09-28）：`max_tokens` 未知即**省略**（`Option` + `skip_serializing_if`）；400 按 `OutputLimit`/`ContextLimit`/`UpstreamUnavailable`/`Other` 分类；被拒后**优先省略**再尝试，流式与非流式共用；解析 `finish_reason`，`"length"` 标记 `truncated`。禁止再把推测值持久化为"学习到的上限"
- **检查方法**：`grep "chat stream rejected\|max_tokens=" app.log`；或直接 `curl` 该网关用超大 `max_tokens` 复现 400
- **触发条件**：网关不报 `capabilities.maxOutput`（agnes）、或聚合网关（如 `coding`）以超大 `max_tokens` 硬发时
- **修复人/时间**：2026-09-28（第三次复发；前两次 `2026-09-19`、`2026-09-10` 均为非流式/局部修复）
- **Tags**: `ai/mod.rs`, `chat_stream`, `max_tokens`, `LlmErrorKind`, `finish_reason`, `聚合网关`

### 3. AI 聊天切页后会话丢失 / 回答不落库 / 停在空「新会话」
- **现象**：聊天途中切到其它页面再回来，会话丢失；聊天完成后侧栏仍停在空「新会话」，看不到聊天记录
- **根因（两处叠加）**：
  1. `App.tsx` 的路由在切页时卸载整个 `AiChat`，`activeId` 全丢；切回时 `ai_capabilities`（快）与 `list_chat_sessions`（慢，读+解析整个 `chat_history.json`）竞态，`sessions` 还没回来就命中 `sessions.length === 0` → **每次新建并激活空会话**
  2. `streamStore`（2026-09-20）只保证「事件不丢」，真正落库的 `applyDone` 仍只在 `ChatPanel` 挂载到**确切 session.id** 时跑；App 乘上新建空会话后，缓冲的 `ai-done` 无人消费。历史越长 `list_chat_sessions` 越慢，竞态越稳定地输
- **修复**（2026-09-29）：`AiChat` 加 `sessionsLoaded` 守卫（列表未回不建会话）+ `sessionStorage(ls_active_chat_session)` 记住/恢复活动会话；`streamStore` 在**无订阅者**（用户已切走）时自行 `load+buildSessionFromDone+save` 兜底落库并标 `persisted`，ChatPanel 恢复时只重拉不追加；pending 清理加 `isPersisting` 守卫防写回竞争。`buildSessionFromDone` 抽为 `ai/applyDone.ts` 纯函数
- **检查方法**：聊天途中切页→等回答完成→切回，确认侧栏高亮的是原会话、回答可见；再 grep `chat_history.json` 对应会话不含空 `assistant` 丢失；或看切回后是否新增空「新会话」条目
- **触发条件**：任何「发送后切页 / 切回聊天页」；历史记录越大越必现
- **修复人/时间**：2026-09-29（第二次修复；2026-09-20 仅修了"事件不丢"，未修"回到原会话"）
- **联带修复（同日）**：删掉最后一个空会话会立刻被 ensure effect 自动重建（表现为「新会话删不掉」）→ 加 `suppressAutoCreateRef`：用户主动删除后不再自动建会话，主面板补「暂无会话 + 新建会话」空态
- **Tags**: `AiChat.tsx`, `ChatPanel.tsx`, `ai/streamStore.ts`, `ai/applyDone.ts`, `sessionsLoaded`, `ls_active_chat_session`, `persisted`, `suppressAutoCreateRef`, 路由卸载

### 4. 聊天内容全部无法持久化（save_chat_session 参数名与前端 invoke 不一致）
- **现象**：会话能建/能列/能删，但问题与回答切页或重启后全部消失；会话永远是空「新会话」、标题不更新。日志里 AI 正常生成回答，但 `chat_history.json` 里 `messages` 始终为空、mtime 停在创建那一刻
- **根因**：`c41f84c` 拆分 `commands/ai.rs` 时把 Tauri 命令 `save_chat_session` 参数名由 `session` 误改为 `sess`。Tauri **按参数名反序列化**，前端一直发 `{ session }` → 每次 `invalid args` 失败；前端 `.catch(() => {})` 静默吞错，长期无人察觉。`create/list/delete/load` 参数名未变，故只有「保存」坏
- **修复**（2026-09-29）：参数改回 `session`；前端保存/删除失败改为 error toast 不再静默（新增 `save_failed`/`delete_failed`）
- **检查方法**：新增/改动 `#[tauri::command]` 后，逐一比对**前端 `invoke('cmd', { key })` 的 key** 与 **Rust 参数名（snake_case → 前端 camelCase）**是否一致；聊天落库类问题先看 `chat_history.json` 的 mtime 是否随发送/回答更新，再 grep 前端 `save session failed`
- **触发条件**：任何 Tauri 命令重命名/拆分重构；此案例自 2026-09-23 静默坏了一周
- **修复人/时间**：2026-09-29
- **Tags**: `commands/ai.rs`, `save_chat_session`, `sess`, Tauri 参数名, `invoke`, 静默吞错

### 5. 提取期长期持有 DB 连接（界面/API 被连接池饿死）
- **现象**：索引期间界面被 Windows 标成"无响应"、前端查询（`[FILES]`）归零，但后台 OCR 照常产出（页计数继续涨）；严重时**批量文件被误判失败**，日志 `DB conn: timed out waiting for connection`
- **根因**：`indexer.rs::extract_and_index_single` 原本**由调用方先取连接再传入**，这条连接从「去重查询」一直持有到「`mark_extracted`」，**横跨整段提取（含数分钟 PDF OCR）**；`batch_io_concurrency` 个 Phase-1 worker 因此在整批期间占满连接池
- **历史误修（必须警惕）**：2026-09-26 与 2026-10-14 **两次都只扩大池 / 延长等待**（池 8→12→24→48、等待 10s→30s→60s），**始终没动"持有期"** → 于是反复复发（第四次才修根因）
- **修复**（2026-10-15，`abb4583`）：签名改为 `(job, db: &Pool<SqliteConnectionManager>, engine_override)`，内部按「A 提取前读 / B 提取（零连接）/ C 提取后写」三阶段**各借各还**；`index_file` 的 `get_dir`/`clear_stale_embeddings`/`update_indexed`/`log_index_error` 同步改短连接
- **铁律**：`crate::load::acquire_doc()` 等闸门许可**必须在取连接之前**取（放在之后会把池占满 —— 55 次超时的成因）
- **池容量怎么定（不要再猜）**：按「同时想借连接的线程数」推导，**不是**按硬件/症状 —— `perf_batch_io_concurrency`（上限 **24**，见 `commands/performance.rs::scale_params`）+ keeper 2 + Web API/扫描/采样若干 ≈ 30；现行 `max_size=48`（≈1.6×）即由此而来。**扩池不是修复**；要动这个数字先给出实测峰值。
- **检查方法**：见到"提取期间界面卡 / `[FILES]` 归零 / `DB conn: timed out`"**先怀疑连接被跨提取期持有**，而不是先扩容；改提取路径后 grep `extract_and_index_single`，确认它**不再接收 `&Connection`**
- **触发条件**：任何新增/改动"提取"路径（批处理、watcher、Web API、verify/重提取、integrity 自愈）；所有重提取入口都汇入 `index_file`
- **修复人/时间**：2026-10-15
- **Tags**: `indexer.rs`, `extract_and_index_single`, `index_file`, `&Pool`, 连接池, `acquire_doc`, 界面无响应
