# 第十二章：自动化测试

> GUI 交互用例 101 个（`test-visual/`，见 [TEST_REPORT](TEST_REPORT.md)）+ Rust 集成测试，覆盖 9 个页面路由 + AI 流式对话全链路。

---

> ⚠️ **历史说明**：下表为较早的 37 用例（15 Mock IPC + 22 MCP）测试体系，已被 101 用例的 `test-visual/` GUI 套件取代；保留供追溯。

Link-Searcher 有两套自动化测试，分别覆盖不同层次：

| 测试套件 | 文件 | 数量 | 运行方式 | 依赖 |
|----------|------|:----:|----------|------|
| Mock IPC 测试 | `src-tauri/tests/auto_ui_e2e.rs` | 15 | `cargo test --test auto_ui_e2e` | 无（可 CI） |
| IPC 命令测试 | `src-tauri/tests/ipc_test.rs` | 8 | `cargo test --test ipc_test` | 无（可 CI） |
| **Web API HTTP 冒烟** | `src-tauri/tests/webapi_http.rs` | 1（多断言） | `cargo test --test webapi_http` | 无（可 CI） |
| 前端会话测试 | `src/api/__tests__/sessionClient.test.ts` | 7 | `npm run test` | 无 |
| MCP E2E 测试 | `src-tauri/tests/e2e_mcp.sh` | 22 | `bash src-tauri/tests/e2e_mcp.sh` | 需 App 窗口 + LLM |

### 提交门禁：一次跑全部目标（不要只跑 `--lib`）

**`cargo test --lib` 不是合格的门禁**——它不编译也不运行 `src-tauri/tests/` 下的集成测试与
mock E2E，于是"测试目标编译不过 / 断言过时"可以长期无人发现（2026-10-12 `ada8485` 让
`auto_ui_e2e` 编译不过、潜伏 3 天；2026-10-15 `2db9c21` 静默打破 `ipc_test`）。详见
`AGENTS.md`「测试门禁」。

```bash
cargo test --manifest-path src-tauri/Cargo.toml --workspace   # 全部目标，约 3 分钟（编译占大头）
npx tsc --noEmit && npx vitest run
```

> ⚠️ **必须带 `--workspace`**：`src-tauri` 既是 workspace 根**又是** package，此时不带参数的
> `cargo test` **只作用于主程序包** → 成员 crate `lark-gate`（飞书机器人准入判定：仅私聊 + 白名单，
> **唯一防线**）与 `lark-bot` 会**静默不跑**。

当前全部目标与实测耗时（本机基线，2026-10-18）：

| 目标 | 用例 | 耗时 | 性质 |
|------|:----:|:----:|------|
| `--lib` | 682（+2 ignored） | 10.1s | 单元 |
| `lark-gate`（成员 crate） | 13 | 0.00s | 准入判定策略（**唯一防线**） |
| `lark-bot`（成员 crate） | 84 | 0.01s | bot 纯函数（回复截断等） |
| `ai_chat_stream_wire` | 15 | 39.7s | mock SSE 网关（最慢，无网络依赖） |
| `auto_ui_e2e` | 15 | 0.5s | mock runtime |
| `ipc_test` | 8 | 5.3s | mock runtime |
| `integration` | 10 | 1.8s | 真实模块 |
| `webapi_http` | 冒烟 | ~0s | 真 axum + 真 Wry（`harness = false`） |
| `focus_mode_test` / `merge_scope_test` / `test_pagination` | 4 / 4 / 2 | <0.1s | 纯逻辑 |
| `doc_rs_poc` / `doc_rs_poc_fallback` | 1 + 1 | 0.00s | POC |
| `test_pdf_inspector_ab` | 2（+1 ignored） | 0.02s | A/B |
| `sensevoice_smoke` / `ocr_eval` / `test_pdf_ocr` | 3 / 1 / 2 | ≤0.25s | **缺样例或 env 时快速早退**，样例存在时才真跑 |
| `remote_embed_adaptive_bench` | 0（1 ignored） | 0.00s | 手动基准，需 `--ignored` + 网络 |

> 换机器/全新克隆后先跑一次 `npm run sidecar`：`bundle.externalBin` 在**编译期**就要求
> `src-tauri/binaries/lark-bot-<triple>.exe` 存在，缺了连 `cargo test` 都会以
> `failed to run custom build command` 失败（看起来像代码坏了，其实是缺文件）。

### Web API HTTP 冒烟测试（`webapi_http.rs`）

真实起 axum（`into_make_service_with_connect_info::<SocketAddr>()`，与生产一致）+ `ureq` 打真请求，覆盖鉴权与在线状态链路：401(无/错 token，且**不创建会话**) → 200 + 首次授予会话（证明来源 IP 抽取成功）→ `/api/session` JSON 形状（含 `tailscale_ip` / `webui_base_url`）→ `/api/search` 形状（含 `semantic_applied`）→ `ping` 刷新在线记录 → `logout` → 重新登入。

两个实现要点（改动前请先读）：
1. `ApiState.app_handle` 是**具体类型** `AppHandle<Wry>`，`MockRuntime` 的 handle 不匹配 → 用 `tauri::Builder::default().manage(state).build(tauri::test::mock_context(tauri::test::noop_assets()))` 构造真实 Wry app（无窗口，headless 可行）。
2. `tao` 在 Windows **禁止非主线程创建事件循环**，libtest 默认在线程里跑测试 → 本文件在 `Cargo.toml` 用 `[[test]] name = "webapi_http" harness = false`，测试体跑在进程主线程。

> 鉴权中间件从 **DB** 的 `app_settings['web_api_token']` 读 token（`ApiState.auth_token` 不参与比对），测试需先写入该行。

### ⚠️ Windows 上的两个已知坑（2026-10-03 已修，勿回退）

上述两个 mock 测试在 Windows 曾**连启动都失败**（`0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND`）或全部命令被拒，原因与修复：

1. **测试二进制缺 comctl32 v6 manifest**：`tauri` GUI 栈会导入 `comctl32!TaskDialogIndirect`（仅 v6 导出）。`tauri_build::build()` 只给主二进制嵌 manifest，`tests/` 下的测试二进制没有 → 加载器按 System32 的 v5.82 绑定 → 进程启动即死。修复在 `build.rs::embed_test_manifest()`，用 `cargo:rustc-link-arg-tests` **只给测试目标**注入 v6 manifest（不影响主二进制）。
2. **mock 请求 URL 平台差异**：`InvokeRequest.url` 必须与平台本地源一致——Windows 是 `http://tauri.localhost`，macOS/Linux 是 `tauri://localhost`。写错会被 ACL 判为 `Remote` 而拒绝所有命令（报 `... not allowed on window "main" ... URL: ...`）。测试里用 `if cfg!(windows) { ... } else { ... }` 处理。

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
- 返回形状（含 `total_files`/`indexed`/`pending`/`errors`/`load_level`）+ 注入状态的 `is_scanning=false`
  > ⚠️ **不能断言 `total_files=0`**：`get_index_status` 刻意走进程级「界面专用只读连接」
  > (`ui_conn_guard`，`2db9c21`) 以避开提取期的连接池竞争，因此**不读**注入的 `AppState` 池，
  > 读到的是真实 data_dir 的统计。同一原因使 `ipc_test::test_ipc_get_index_status` 也如此断言。

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
cd <项目根目录>
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