# 2026-11-02 重回答后 UI 卡死（未修复）

> 状态：**OPEN / 必须修复**
> 影响版本：v1.4.0（渐进检索 v3）
> 记录时间：2026-11-02

## 现象

用户按测试集运行到第 4 条：

```text
把万城公司的工商变更逐条全部列出，不要总结、不要省略
```

后端回答完成（2236 字，19 篇材料），但回答结束后整个窗口无响应：

- `link-searcher.exe` 的 `Responding=False`
- CPU 基本不再增长（2 秒仅 +0.016s）
- 窗口存在且 `enabled=True`
- 无模态对话框
- 重启应用后恢复

## 现场证据

- `app.log` 最后一条为 `2026-11-02 09:31:43`：
  - `chat_stream returned: chars=2236`
  - `AI_TRACE turn_end`
  - `SLOW-IPC conversation_ask_stream took 69445ms`
- 09:31:43 之后 `app.log` 没有任何新记录。
- 最后一轮回答已写入 `chat_history.json`，`pending_query=false`，数据未丢失。
- 数据库读写探测正常，未锁死。
- 日志中无 panic / ERROR。
- 该轮事件只有 8 条，payload 很小：
  - `query_rewrite`
  - `scope_resolved`
  - `sufficiency_check`
  - `retrieval`
  - `context_assembled`
  - `llm_call`
  - `citation_check`
  - `turn_complete`
- 该轮未触发 `progressive_round`（`sufficiency_check` 直接 sufficient）。

## 判断

**这不是检索/LLM 慢导致的卡住**：后端已经完成并落库，卡在回答完成后的 UI / WebView / IPC 层。

69 秒是“重回答的耗时”，属于慢；09:31:43 之后 17 分钟无响应是另一个问题。

## 当前结构性风险

后端顺序：

```text
chat_stream 结束
  → citation_stats
  → 写 ai_events
  → auto_cite
  → app.emit("ai-done")
```

即：**UI 必须等所有后处理完成才能收到 `ai-done`**。任何一步阻塞，界面都会停在“生成中”。

前端风险：

- `streamStore.handleDone` 的 `notify` 没有逐个 listener 的 `try/catch`；
- 某个 listener 抛异常可能让状态清理不完整；
- 事件时间线当前是回答后主动拉取，重回答会增加完成瞬间的 IPC/渲染压力。

## 修复方案（待实施）

1. 后端加检查点日志：
   - `citation_start/done`
   - `events_start/done`
   - `autocite_start/done`
   - `emit_done_start/done`
2. `ai-done` 不再等待分析型后处理：
   - `auto_cite` 放 `spawn_blocking` + 超时；
   - `ai_events` 写入改成后台任务；
   - 先保证 UI 收到完成事件。
3. 前端加看门狗：
   - 流式态超时自动清理；
   - 提示“回答可能已完成但同步失败，请重载会话”；
   - 不阻塞后续操作。
4. `handleDone` 加 `try/finally`，listener 单独 try/catch。
5. 事件时间线改为展开时懒加载。

## 验收标准

- 重回答（19 篇/138K 上下文/2K+ 回答）完成后，窗口保持 `Responding=True`；
- 即使后处理阻塞，UI 也能在超时后恢复输入；
- `ai-done` 发出前不再有不可中断的同步后处理；
- 复现时能从日志定位到具体卡住阶段。
