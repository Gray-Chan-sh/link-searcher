# 第十二章：自动化测试

> 37 个 E2E 用例，覆盖 8 个页面路由 + AI 流式对话全链路。

---

Link-Searcher 有两套自动化测试，分别覆盖不同层次：

| 测试套件 | 文件 | 数量 | 运行方式 | 依赖 |
|----------|------|:----:|----------|------|
| Mock IPC 测试 | `src-tauri/tests/auto_ui_e2e.rs` | 15 | `cargo test --test auto_ui_e2e` | 无（可 CI） |
| MCP E2E 测试 | `src-tauri/tests/e2e_mcp.sh` | 22 | `bash src-tauri/tests/e2e_mcp.sh` | 需 App 窗口 + LLM |

---

## Mock IPC 测试（15 个）

基于 `tauri::test` mock 运行时，无需真实窗口。覆盖：

### 搜索
- 空搜索返回空结果集
- 搜索建议空前缀返回空列表

### 浏览
- 空文件列表

### 资料库
- 空目录列表
- 添加目录
- 删除目录

### 索引状态
- 初始状态（total_files=0, indexed=0）

### 设置
- 读取设置
- 更新设置（OCR 语言）
- 配置读取

### 日志
- 日志页面（最佳 effort）

### 文件类型
- OCR 引擎列表
- 文件类型支持

### AI 聊天
- 会话 CRUD（创建/列表/加载/保存/导出/删除）
- AI 能力探测

### 版本
- 版本号不为空

## MCP E2E 测试（22 个）

通过 `tauri-plugin-mcp` 连接真实 App 窗口，操作真实 DOM。覆盖：

### 搜索页（3 个）
- 搜索页加载成功
- 搜索页内容已渲染（"搜索您的文档"）
- 语义搜索入口存在

### 浏览页（1 个）
- 浏览页加载成功

### 资料库页（1 个）
- 资料库页加载成功

### 索引状态页（3 个）
- 索引状态页加载成功
- 索引统计信息存在（"已索引"）
- 扫描按钮存在

### 文件类型页（1 个）
- 文件类型页加载成功

### 设置页（2 个）
- 设置页加载成功
- OCR 设置项存在

### 日志页（1 个）
- 日志页加载成功

### 主题切换（1 个）
- 主题按钮可点击

### AI 聊天（9 个）
- AI 聊天页加载成功
- 聊天输入框存在
- 输入问题成功
- 发送按钮点击成功
- AI 流式响应完成（超时 30s）
- AI 回答包含关键词
- 响应耗时显示（⏱）
- 展开检索依据面板
- 证据面板存在

---

## 运行方式

### 终端 1：启动调试 App

```bash
cd /Volumes/Data/Project/Link-Searcher
npx tauri dev
```

等待 App 窗口出现，MCP socket 自动监听。

### 终端 2：运行 MCP E2E 测试

```bash
bash src-tauri/tests/e2e_mcp.sh
```

预期输出：

```
✅ MCP socket: /var/folders/.../T/tauri-mcp.sock
=== [1] 搜索页 ===
  ✅ 1.1 搜索页加载
  ✅ 1.2 搜索页内容已渲染
  ✅ 1.3 语义搜索入口
...
  结果: 22 通过, 0 失败
```

### 运行 Mock IPC 测试（无需 App）

```bash
cd src-tauri
cargo test --test auto_ui_e2e
```

---

## 覆盖报告

完整覆盖报告见 `docs/e2e-coverage-report.md`。

---

## 测试设计原则

1. **Mock 测试可 CI**：不依赖窗口、LLM 网关、OS 集成，15 个用例全部通过
2. **MCP 测试覆盖真实交互**：打字、点击、流式响应、证据面板，全部基于真实 DOM
3. **AI 流式测试有超时保护**：最多等待 30 秒，避免测试挂死
4. **测试隔离**：每个 Mock 测试使用独立 temp 目录，自动清理

---

## 常见问题

### Q：MCP 测试找不到 socket？

确保 App 是 debug 构建已启动，且 `tauri-plugin-mcp` 已加载：

```bash
# 检查 socket 文件
find /var/folders -name "tauri-mcp.sock" 2>/dev/null

# 检查 App 进程
ps aux | grep "link-searcher" | grep -v grep
```

### Q：AI 流式测试超时？

可能原因：
1. LLM 网关未配置或不可用
2. 网关响应慢（超过 30 秒）
3. 输入框选择器不匹配（检查 placeholder 文本）

### Q：Mock 测试失败？

检查 `cargo test --test auto_ui_e2e` 的完整输出，确认是代码变更导致的断言失败还是环境问题。