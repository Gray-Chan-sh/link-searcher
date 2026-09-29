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
  ↓
6. 提交 & 推送：git commit + git push（CHANGELOG 必须同在此 commit）
```

> **例外**：纯修 typo、单行 config 改值、仅文档修改等**微不足道**的变更，可跳过步骤 2–3，直接实施。

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
