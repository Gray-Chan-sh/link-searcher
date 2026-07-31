# Link-Searcher Agent 规范

> 每次进行代码变更时自动读取，确保一致性。

## 变更记录

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
