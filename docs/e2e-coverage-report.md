# E2E 测试覆盖报告

> 生成时间：2026-08-14
> 测试文件：`src-tauri/tests/auto_ui_e2e.rs` + `src-tauri/tests/e2e_mcp.sh`
> 运行命令：
> - `cargo test --test auto_ui_e2e`（15 个 mock IPC 测试）
> - `bash src-tauri/tests/e2e_mcp.sh`（22 个 MCP E2E 测试，需 App 运行）

---

## 测试结果汇总

| 测试套件 | 类型 | 通过 | 失败 | 覆盖范围 |
|----------|------|:----:|:----:|----------|
| `auto_ui_e2e.rs` | Mock IPC | 15 | 0 | 无窗口的 IPC 逻辑 |
| `e2e_mcp.sh` | MCP E2E | 22 | 0 | 真实窗口 + DOM 交互 |
| **总计** | | **37** | **0** | |

---

## 完整覆盖清单

### 搜索页

| 功能点 | 测试方式 | 状态 |
|--------|---------|:----:|
| 搜索页加载 | MCP navigate + query_page | ✅ |
| 搜索页内容渲染 | MCP execute_js 验证"搜索您的文档" | ✅ |
| 语义搜索入口 | MCP execute_js 验证"语义"按钮 | ✅ |
| 空搜索 | Mock IPC `search` | ✅ |
| 搜索建议 | Mock IPC `suggest` | ✅ |
| 搜索框输入文字 | MCP type_text | ✅ |
| 回车触发搜索 | MCP press_key(Enter) | ✅ |

### 浏览页

| 功能点 | 测试方式 | 状态 |
|--------|---------|:----:|
| 浏览页加载 | MCP navigate + query_page | ✅ |
| 空文件列表 | Mock IPC `list_files` | ✅ |

### 资料库页

| 功能点 | 测试方式 | 状态 |
|--------|---------|:----:|
| 资料库页加载 | MCP navigate + query_page | ✅ |
| 空目录列表 | Mock IPC `list_dirs` | ✅ |
| 添加目录 | Mock IPC `add_dir` | ✅ |
| 删除目录 | Mock IPC `remove_dir` | ✅ |

### 索引状态页

| 功能点 | 测试方式 | 状态 |
|--------|---------|:----:|
| 索引状态页加载 | MCP navigate + query_page | ✅ |
| 索引统计信息 | MCP execute_js 验证"已索引" | ✅ |
| 扫描按钮存在 | MCP execute_js 验证"开始扫描" | ✅ |
| 索引状态读取 | Mock IPC `get_index_status` | ✅ |

### 文件类型页

| 功能点 | 测试方式 | 状态 |
|--------|---------|:----:|
| 文件类型页加载 | MCP navigate + query_page | ✅ |
| 文件类型支持列表 | Mock IPC `get_file_type_support` | ✅ |
| OCR 引擎列表 | Mock IPC `list_ocr_engines` | ✅ |

### 设置页

| 功能点 | 测试方式 | 状态 |
|--------|---------|:----:|
| 设置页加载 | MCP navigate + query_page | ✅ |
| OCR 设置项存在 | MCP execute_js 验证"OCR" | ✅ |
| 设置读取 | Mock IPC `get_settings` | ✅ |
| 设置更新 | Mock IPC `update_settings` | ✅ |
| 配置读取 | Mock IPC `get_config` | ✅ |

### 日志页

| 功能点 | 测试方式 | 状态 |
|--------|---------|:----:|
| 日志页加载 | MCP navigate + query_page | ✅ |

### 主题切换

| 功能点 | 测试方式 | 状态 |
|--------|---------|:----:|
| 主题按钮点击 | MCP click(文本匹配) | ✅ |

### AI 聊天

| 功能点 | 测试方式 | 状态 |
|--------|---------|:----:|
| AI 聊天页加载 | MCP navigate + query_page | ✅ |
| 聊天输入框存在 | MCP execute_js 验证输入框 | ✅ |
| 输入问题 | MCP type_text 到输入框 | ✅ |
| 点击发送按钮 | MCP click(文本匹配"发送") | ✅ |
| 等待 AI 流式响应 | MCP 轮询 execute_js（超时 30s） | ✅ |
| AI 回答验证 | MCP execute_js 验证"营收"关键词 | ✅ |
| 响应耗时显示 | MCP execute_js 验证"⏱"标识 | ✅ |
| 展开检索依据面板 | MCP click(文本匹配"检索依据") | ✅ |
| 证据面板存在 | MCP execute_js 验证"检索依据" | ✅ |
| 会话创建 | Mock IPC `create_chat_session` | ✅ |
| 会话列表 | Mock IPC `list_chat_sessions` | ✅ |
| 会话加载 | Mock IPC `load_chat_session` | ✅ |
| 会话保存（标题更新） | Mock IPC `save_chat_session` | ✅ |
| 会话导出 | Mock IPC `export_chat_session` | ✅ |
| 会话删除 | Mock IPC `delete_chat_session` | ✅ |
| AI 能力探测 | Mock IPC `ai_capabilities` | ✅ |

### 版本信息

| 功能点 | 测试方式 | 状态 |
|--------|---------|:----:|
| 应用版本号 | Mock IPC `get_version` | ✅ |

---

## 不可自动覆盖的场景

| 场景 | 原因 |
|------|------|
| 触发扫描/重建索引 | 需要 `AppHandle`（mock 不支持） |
| 索引完整性检查 | 需要 `AppHandle`（mock 不支持） |
| OCR 识别 | 需要 PaddleOCR 模型预先初始化 |
| 系统托盘 | 需要 OS 级集成 |
| 弹窗/确认对话框 | 需要 Tauri dialog 插件上下文 |
| 文件拖拽 | 需要 OS 级事件 |

---

## 风险注释

1. **MCP 测试依赖真实 App 窗口**：仅本地 macOS debug 环境可运行
2. **Mock 测试可 CI 运行**：无窗口依赖，15 个测试全部通过
3. **tauri-plugin-mcp 仅 debug 构建**：已通过 `#[cfg(debug_assertions)]` 确保
4. **AI 流式测试依赖 LLM 网关**：需要有效 LLM 配置才能通过

---

## 使用方式

```bash
# Mock IPC 测试（无窗口，可 CI）
cd src-tauri && cargo test --test auto_ui_e2e

# MCP E2E 测试（需要先启动 App）
npx tauri dev                    # 终端 1：启动调试 App
bash src-tauri/tests/e2e_mcp.sh  # 终端 2：运行 E2E 测试