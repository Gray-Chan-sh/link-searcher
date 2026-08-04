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
6. 提交 & 推送：git commit + git push
```

> **例外**：纯修 typo、单行 config 改值、仅文档修改等**微不足道**的变更，可跳过步骤 2–3，直接实施。

---

## 提交流程（每次 commit 前必做）

**⚠️ 任何源代码变更（含 bug 修复、功能、重构）必须在同一个 commit 中更新 CHANGELOG.md。**
**漏写 CHANGELOG 的 commit 视为 incomplete，不可推送。**

每次 commit 前，**必须按以下顺序完成**：

```
1. 更新 CHANGELOG.md   → 记录本次变更（根因、怎么修、涉及哪些文件）  ← 强制，不可跳过
2. 更新 README.md       → 如涉及功能/架构变化，同步文档
3. 更新 USER_MANUAL.md  → 如涉及用户可见行为变化，同步手册
3.5. Semgrep 检查 → semgrep scan --severity ERROR 零发现
4. 编译验证             → cargo check / npx tsc 零错误
5. 提交并推送           → git commit + git push（CHANGELOG 必须同在此 commit）
```

## 静态分析（Semgrep）

每次提交前，必须运行阻塞级检查，**零发现**才可提交：

```bash
semgrep scan \
  --config .semgrep/custom.yml \
  --config p/owasp-top-ten \
  --config p/secrets \
  --severity ERROR
```

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
| `src-tauri/src/lib.rs` | Tauri 初始化 + 启动流程 |
| `src-tauri/src/scanner/mod.rs` | 全量/增量/启动扫描 |
| `src-tauri/src/indexer.rs` | 索引服务（batch_index / MD5 / 去重） |
| `src-tauri/src/db/tracker.rs` | 文件追踪 CRUD + 统计 |
| `src-tauri/src/extractor/paddleocr.rs` | PaddleOCR 内置引擎 |
| `src/pages/SearchPage.tsx` | 搜索页 |
| `src/pages/Browse.tsx` | 浏览页（表格视图） |
| `src/pages/IndexStatus.tsx` | 索引状态页 |
| `src/pages/Settings.tsx` | 设置页 |
| `CHANGELOG.md` | 变更日志（每次 commit 必更新） |
