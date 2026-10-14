# 飞书机器人接入（设计记录 · 未评审）

> 状态：**讨论记录**，未经方案评审，不代表会实施。
> 最后更新：2026-10-15
>
> 关联文档：[`EXTENSION_IDEAS.md`](./EXTENSION_IDEAS.md)（扩展机制与功能清单，本方案属其
> "D. 对外集成类"）。本文只记录已探明的技术事实、可行方案与待确认项，**不含代码改动计划**。
>
> **2026-10-15 接口核对修订**：`POST /api/chat/ask/stream` 不存在 → 流式应为
> `POST /api/ai/conversation/ask/stream`；`/api/ai/events*` 是普通 GET 而非 SSE；
> §4.3 耦合点计数 11 个文件/~85 处 → 12 个 route 模块/71 处。方案 A/B 的可行性结论不变。

---

## 一、目标与定位

把 Link-Searcher 的本地私有库接到飞书：群里 @机器人 提问、随手丢文件入库、定时汇报。

**先划清楚定位**，它决定后面所有取舍：

| 定位 | 可行性 | 代价 |
|---|---|---|
| **个人助手**（自己问、自己存） | ✅ 推荐先做 | 机器睡了/退了就离线 |
| 团队 7×24 服务 | ⚠️ 要额外工作 | 需要 headless 常驻 + 数据边界设计 |

---

## 二、为什么不用公网（关键前提）

飞书事件订阅有两条路：

| 方式 | 要求 | 适合本地桌面程序？ |
|---|---|---|
| Webhook（HTTP 回调） | 公网 HTTPS 域名 + 证书 | ❌ |
| **长连接（WebSocket）** | 程序主动连出去 | ✅ |

**已验证**（本机 2026-10-15）：

- `lark-cli 1.0.96` 已安装（`C:\Users\Gray\AppData\Local\Programs\nodejs\lark-cli`）
- `lark-cli event` 提供统一事件消费：`event consume <EventKey>` 把事件以 **NDJSON** 流输出到 stdout
- 由本地 **event bus 守护进程**（UDS 本地 IPC）承接长连接，**不需要公网 IP / 域名 / 证书**
- 面向子进程消费的契约完备：
  - stderr 就绪标记：`[event] ready event_key=<key>`
  - 边界退出：`--max-events N` / `--timeout D`（两者先到先退出）
  - `--quiet` 会隐藏事件丢失诊断，**AI 消费者不应使用**
  - `--as user|bot|auto` 切换身份
  - 退出码分级（0 正常 / 1 业务失败 / 2 校验失败 / 3 鉴权失败 / 4-5 网络内部）
  - **不要 `kill -9`**：部分 EventKey（minutes/vc/board）退出时会向服务端退订，`kill -9`
    会泄漏服务端订阅，表现为重启后 "subscription already exists" 与重复投递

**当前状态**：`lark-cli auth status` 返回 `not_configured`。
前置步骤：`lark-cli config init --new`（需在飞书开放平台建**自建应用**，拿 app_id/app_secret，
走一次 OAuth 授权）。

相关命令：

```bash
lark-cli event list --domain im --json        # 列出可订阅 EventKey
lark-cli event schema im.message.receive_v1   # 看字段路径（jq_root_path）
lark-cli event consume im.message.receive_v1 --as bot
```

---

## 三、能力边界：能干什么，不能干什么

### 3.1 前提结论：webapi 覆盖了几乎所有功能

`webapi/static_files.rs` 把**完整前端**一起发出去（`serve_from_dist` + SPA fallback 到
`index.html`）——浏览器打开就是完整 Web 版应用。

因此机器人有两条路：

- **轻交互** → 飞书里回卡片/文本
- **重交互** → 卡片上挂一个**进 WebUI 的链接**（预览、浏览、批量管理）

**机器人不需要复刻全部功能**，它只要"能问答 + 能给链接"。

### 3.2 AI 聊天：全功能可用

| 能力 | 路由 |
|---|---|
| 提问（单轮，非流式） | `POST /api/chat/ask` |
| 多轮会话提问（非流式） | `POST /api/ai/conversation/ask` |
| **流式回答**（SSE，按 session 推流） | `POST /api/ai/conversation/ask/stream` |
| 智能搜索流式（SSE） | `POST /api/ai/smart-search/stream` |
| 会话 列/建/载/存/删/导出 | `/api/chat/sessions*`（含 `POST /api/chat/sessions/{id}/export`） |
| 摘要 | `POST /api/ai/summarize` |
| 智能搜索（非流式） | `POST /api/ai/smart-search` |
| 网关测试 | `GET /api/ai/gateways/test` |
| 取消生成 | `POST /api/ai/cancel` |
| 推理过程事件 | `GET /api/ai/events?session_id=`、`GET /api/ai/events/turn`（**普通 JSON，非 SSE**，读已落库事件） |
| 能力探测 | `GET /api/ai/capabilities` |

> ⚠️ **不存在 `POST /api/chat/ask/stream`**（2026-10-15 核对修正）。要流式只能走上面两条
> `/stream`（SSE）；`/api/ai/events*` 是普通 GET，只能轮询已落库事件，不能当进度推送用。
> 对应的实现约束见 §八「慢回答的体验」。

→ 多轮会话可天然映射到飞书：**每个群 / 每个私聊绑一个 session id**。

### 3.3 其它可用能力（90+ 路由）

| 类别 | 可用能力 |
|---|---|
| 搜索 | `search` / `suggest` / `refine` / `export` / `history` / `paths` / `file-ids` |
| 浏览预览 | `files` / `files/browse` / `files/{id}` / `files/{id}/preview` / `dir-entries` / `dirs/tree` |
| 索引运维 | `index/status`、`health`、`scan/trigger`、`scan/cancel`、`reindex`、`rebuild`、`reindex-batch`、`reextract`、`verify`、`retry-failed`、`errors`、`integrity`、`heal` |
| 备份 | `backup/trigger`、`status`、`list`、`export`、`restore`、`restore-zip`、`dead-dirs`、`remap` |
| 依赖安装 | `setup/status`、`install`、`install-status`、`cancel`（模型按需下载也能远程触发） |
| OCR | `ocr/engines`、`engines/test`、`dependencies`、`file-type-support` |
| 配置 | `config` GET/PUT、`providers`、`active-model`、`migrate` |
| 系统 | `version`、`system/restart`、`auth/token`、`session`、`logs`、`events`(SSE) |

### 3.4 明确不可用（仅 3 条）

`webapi/routes/files.rs:37-39` 三条路由是 **`desktop_only_stub`**（占位桩）：

| 能力 | 状态 | 原因 |
|---|---|---|
| 用系统程序打开文件 | ❌ | 要桌面 GUI |
| 在资源管理器里定位 | ❌ | 同上 |
| 原生下载对话框 | ❌ | 同上 |

这三条对机器人**本就无意义**（对话里"打开文件"没有所指），且可用 WebUI 链接替代。
（`files/{id}/preview` 可用，预览内容能拿到。）

其它结构性限制：需要裸机 GUI 交互的（原生选目录对话框、系统托盘、开机自启、主题跟随系统）
在机器人侧不适用，但都不影响"问答 + 浏览 + 管理"这条主线。

---

## 四、架构

### 4.1 数据流

```
飞书云端 ──长连接(WebSocket)──> lark-cli event bus daemon
                                      │ NDJSON (stdout)
                                      ↓
                            bot 进程（消费 im.message.receive_v1 等）
                                      │ HTTP + Bearer token
                                      ↓
                            Link-Searcher webapi (HTTPS, 默认 :8443)
                                      │
                            飞书 OpenAPI（回卡片/消息）──> 群里
```

### 4.2 两种接法

| 方案 | 机器人进程 | 侵入主程序 | 评价 |
|---|---|---|---|
| **A. sidecar** | 独立进程，消费 event + 调 webapi | 无 | ✅ 推荐，与其它扩展同一套机制 |
| B. 内嵌主程序 | 主程序内加消费者 | 高 | 少一跳，但绑死主程序 |
| C. 定时轮询 | 无 | 无 | 不需要实时性时最简，但没法"@就问" |

### 4.3 webapi 与 Tauri 的耦合（决定实现路径）

**关键事实**：webapi **不是**独立服务，它跑在 Tauri 桌面进程里，且深度耦合 AppHandle。

| 耦合点 | 位置 |
|---|---|
| `spawn_server(app_handle: tauri::AppHandle)` | `webapi/mod.rs:48` |
| 在 Tauri setup 内调用：`webapi::spawn_server(app.handle().clone())` | `lib.rs:424` |
| `ApiState.app_handle: tauri::AppHandle` | `webapi/state.rs:6` |
| `state.app_handle.state::<AppState>()` | 12 个 route 模块（+ `webapi/session.rs`），**71 处** |
| **`AppHandle` 被传进 `commands::*` 当参数** | 如 `commands::dirs::remove_dir(app_state, app_handle, ...)` |
| `app_handle.emit(...)` 发进度事件 | index/ai 路由 |
| `listen_any` 事件桥 + `async_runtime::spawn` | `webapi/mod.rs:62-69, 107` |

→ **不要试图解耦 webapi**：第 5 行意味着要改**整个 commands 层的签名** + 上百个调用点，高风险。

→ 正确做法不是解耦，而是**给它一个真的 AppHandle**（见 5.1 方案 B）。

### 4.4 事件桥

`webapi/mod.rs:33-46` 定义 `BRIDGED_EVENTS`，把 Tauri 事件转发给 Web 客户端（SSE）：

```
scan-progress / scan-completed / ai-chunk / ai-progress / ai-done
migration-progress / migration-warning / funasr-install-done / bge-install-done
restore-completed / dep-progress / dep-install-done
```

机器人可以用 `/api/events`（SSE）拿进度，做"处理中"卡片。

---

## 五、部署形态两方案

### 5.1 方案 A：要求用户"开机 + GUI 常驻"（推荐先做，几乎零改动）

**项目已具备的能力**：

| 需要的能力 | 现状 | 依据 |
|---|---|---|
| webapi 常开 | ✅ **默认就是开的**（`web_api_enabled != "false"` 才关） | `lib.rs:408-427` |
| 关窗口不退出 | ✅ 拦截 `CloseRequested` → `prevent_close()` + `window.hide()` | `lib.rs:706` |
| 托盘常驻 + 退出入口 | ✅ 托盘菜单「显示/隐藏」「退出」，`quit` → `app.exit(0)` | `lib.rs:625-673` |
| 开机自启 | ⚠️ 插件装好、权限授了，**但前端没接线** | `Cargo.toml:32`、`capabilities/default.json:25` |

→ **不需要** headless `serve`、**不需要** data_dir 锁、**不需要**解耦（天然单进程）。

**好处**：用"用户桌面"换"零代码改动"。**代价**：只能算个人助手，不是 7×24 服务。

**待补的 4 个小缺口**：

1. **autostart 没接线**（唯一要写代码的地方）
   插件已装、权限已授（`autostart:default`），但在 `src/` 全局搜 `autostart`（含 import 路径）
   **零命中** → 前端从未调用 `enable()` / `isEnabled()`。
   → 需在设置页加开关（`@tauri-apps/plugin-autostart`）。
2. **webapi 默认绑 `0.0.0.0`**
   `settings_bind()` 默认值即 `"0.0.0.0"`（`webapi/mod.rs:225-236`）→ **默认对局域网开放**。
   个人隐私库这个默认偏激进，建议引导用户显式选择（同机机器人用 `127.0.0.1` 即可）。
3. **token 从哪读**（不用改代码）
   `resolve_token()`（`webapi/mod.rs:148`）优先级 **env `LS_WEB_API_TOKEN` > `config.json` > DB >
   首次生成**，并始终回写 `config.json` → 机器人直接读该文件。
4. **"退出"是唯一死法**
   托盘「退出」→ `app.exit(0)` → webapi 与机器人一起死。可接受，但机器人侧要能**感知离线并告警**。

**必须接受的 4 条运行契约**（要写进用户说明）：

| 契约 | 原因 |
|---|---|
| 机器开机 + **保持登录会话** | autostart 是 Windows **登录级** Run key，不是服务；没人登录就不跑 |
| **不能休眠/睡眠** | 睡眠挂起进程、长连接断开；唤醒能重连，但睡眠期间消息可能丢 |
| **GUI 必须常驻**（关窗口可以，托盘退出不行） | webapi 跑在 Tauri 进程里 |
| 升级/重启期间机器人短暂离线 | 需幂等 + 重试 |

**最优形态：GUI 拉起 bot sidecar**

- `tauri-plugin-shell = "2"` **已引入**（`Cargo.toml:30`）
- Tauri 2 **sidecar**（`tauri.conf.json` 声明 `bundle.externalBin`）可做到：
  **GUI 启动 → bot 启动；GUI 退出 → bot 退出**
- 好处：用户装一个 app 就全有了；生命周期天然对齐，**不需要看门狗**与进程互斥
- bot 侧只需自行处理 webapi 短暂不可用（重启窗口）

### 5.2 方案 B：headless `serve` 模式（不想开 GUI 时）

**可行性基础**：

- `bootstrap_core(data_dir) -> Bootstrap` **完全不含 Tauri**；`boot.rs:1-4` 注释明确说明
  "so both the Tauri GUI (`lib.rs`) and the CLI (`cli.rs`) reuse identical assembly"
- `main.rs:4-29` 已是参数分发表（`--data-dir X <cmd>` → CLI；无参 → GUI；其它 → CLI）
  → 加一个 `serve` 子命令很自然

**做法**：`serve` = **无窗口的 Tauri 实例**（不是解耦）。

```
link-searcher --data-dir D:\data serve
        ↓
 run_headless()          ← 与 run() 同一 builder
        ├─ setup 照常：app.manage(AppState)、webapi::spawn_server、watcher、启动扫描
        ├─ ✗ 跳过 WebviewWindow 创建
        ├─ ✗ 跳过托盘 / close-to-tray（或保留托盘作退出入口）
        └─ app.run(|_,_| {})   ← 事件循环常驻
```

好处：routes / commands / 事件 emit / WebUI 静态文件**全部不用改**；
webapi 拿到的仍是真 `AppHandle`，71 处调用与 `commands::*` 签名一个字不动。

改动面：

| 改动 | 文件 | 规模 |
|---|---|---|
| 加 `serve` 分发 | `main.rs` | 几行 |
| 抽出 `run_with(opts)`，window/tray 条件化 | `lib.rs` | 局部重构 |
| data_dir 单实例锁 | 新增小模块 | 小 |
| webapi / commands 解耦 | — | **0（不做）** |

**硬约束：GUI 与 headless 必须互斥**（同一 data_dir）

| 风险 | 依据 | 后果 |
|---|---|---|
| Tantivy writer 二次打开 | `search/mod.rs:153` 注释明说 "Returns an error if another writer is already open" | 报错（温和） |
| **`open_or_create` schema 不匹配时 `remove_dir_all(index_dir)` 重建** | `search/mod.rs:85-96` | **索引被删** ⚠️ |
| SQLite 双写者 | r2d2 + WAL，`busy_timeout` 15s | 写冲突/超时 |
| 破坏性路由（`reindex`/`rebuild`/`restore`/`restore-zip`/`backup/trigger`） | 两边同时跑 | 直接毁数据 |

→ 必须加 **data_dir 级单实例锁**（`.lock` 文件独占打开）。
注意：已有的 `tauri-plugin-single-instance` **只按应用标识、不按 data_dir**，因此不够用。

**平台限制（决定方案 B 能不能用）**：

| 平台 | 无窗口 Tauri | 说明 |
|---|---|---|
| **Windows** | ✅ | tao 用 Win32 消息循环，**不需要窗口** |
| Linux | ⚠️ | tao/GTK 需要 X11/Wayland，**纯无头服务器跑不起来**（除非 `xvfb-run`） |
| macOS | ⚠️ | 需要 GUI 会话（launchd agent） |

→ 若目标机器是 **Linux NAS**，方案 B 不成立，只能做"真解耦"（工作量远大于方案 A/B）。

---

## 六、能干什么（功能清单）

按"复用已有能力"排序，⭐ 表示复用程度。

### 强复用（几乎白送）

| 功能 | 复用 |
|---|---|
| **群里 @机器人 提问** → RAG 回答 + 引用来源（核心） | ⭐⭐⭐ |
| **搜索卡片**：「搜 合同」→ 卡片列出命中 + 摘要 + 按钮 | ⭐⭐⭐ |
| **文件入站**：群里发文件/转发 → `drive` 下载 → 监控目录 → **watcher 自动增量索引** → 回「已入库」 | ⭐⭐⭐ |
| **语音入站** → FunASR 转写 → 入库/回答 | ⭐⭐⭐ |
| **图片入站** → OCR → 入库 | ⭐⭐⭐ |
| **定时摘要/告警** → 每日增量汇总、低质量文件告警（`quality_flags`）、索引健康（`health`） | ⭐⭐ |

### 中复用

| 功能 | 复用 |
|---|---|
| **反向出口**：检索结果写成飞书文档/表格，或发成群文件（`docs`/`sheets`/`wiki` 域） | ⭐⭐ |
| **会议纪要入站**：飞书妙记/会议记录 → 本地知识库（`minutes`/`vc`/`note` 域） | ⭐⭐ |
| **卡片回调**：按钮"追问/反馈/打开详情"（`card` 域事件） | ⭐⭐ |

### 弱复用（要额外业务逻辑）

| 功能 | 复用 |
|---|---|
| **审批联动**：审批通过 → 自动归档入指定目录（`approval` 域） | ⭐ |
| **任务联动**：文档里抽出的待办 → 建飞书任务（`task` 域） | ⭐ |

### 可订阅的事件域

`application`、`approval`、`board`、`card`、`im`、`minutes`、`task`、`vc`

---

## 七、安全与数据边界（最重要，先于实现）

把本地私有库的内容发到飞书 = **内容离开本机上云**。企业文档很可能不允许。

**必须能配置**：

1. **外发白名单**：哪些目录 / 哪些文件允许被机器人引用
2. **只回摘要不回原文**：默认模式下不把正文原样贴到飞书
3. **外发前脱敏**：身份证 / 银行卡 / 手机号 / 密钥（可复用 `EXTENSION_IDEAS.md` 中"敏感信息扫描"扩展
   的闸门）
4. **访问控制**：谁能问？群 / 用户白名单；避免把敏感库暴露给不相关的人
5. **凭据存储**：app_secret 不得硬编码（webapi 侧已有 `config.json` + Bearer token 先例可参照）

> **这一条不解决，不建议做。**

---

## 八、另外三个必须处理的技术点

| 问题 | 说明 | 做法 |
|---|---|---|
| **慢回答的体验** | RAG 要几秒到几十秒；飞书有消息长度、卡片更新次数、QPS 限制 | 先回"处理中"卡片 → 用 patch 逐步更新。**能用的 SSE 只有两条**：`POST /api/ai/conversation/ask/stream`（按 session 推流）与全局 `GET /api/events`（含 `ai-chunk`/`ai-progress`/`ai-done`）。`/api/ai/events` 是普通 GET，只能轮询已落库事件 |
| **幂等** | 事件可能重投（官方文档明确提到订阅泄漏会导致重复投递） | 按 `message_id` 去重，否则重复回答 |
| **离线感知** | GUI 退出/重启时 webapi 不可用，机器人会"静默失联" | 健康检查 + 向指定群告警 |

---

## 九、分阶段实施建议

| 阶段 | 内容 | 前置 |
|---|---|---|
| **P0** | `lark-cli config init` 建自建应用并授权；`event consume` 拿到 `im.message.receive_v1` 样本，确认字段 | 管理员审批 scopes |
| **P1** | 白名单群里 @机器人提问 → `/api/chat/ask` → 回卡片；**autostart 开关**补上；只回摘要 | 数据边界规则定好 |
| **P2** | 文件 / 语音 / 图片入站（下载 → 监控目录 → 自动入库 → 回执） | P1 稳定 |
| **P3** | 定时摘要 + 出口（写飞书文档/表格）+ 卡片回调 | — |
| **P4**（可选） | headless `serve` 模式（方案 B），用于无 GUI 的机器 | 明确目标机器平台 |

---

## 十、待确认事项

- [ ] **定位**：个人助手（方案 A）还是团队 7×24 服务？
- [ ] **目标机器**：Windows 桌面常驻 / Windows 服务 / Linux NAS？（Linux → 方案 B 不成立）
- [ ] **数据边界**：哪些目录可外发？是否只回摘要？是否要脱敏闸门？
- [ ] **身份**：`--as bot` 还是 `--as user`（后者权限更大更敏感）？
- [ ] **访问控制**：哪些群 / 哪些人可用？
- [ ] 是否接受"GUI 与 headless 互斥"，还是要求共存（需只读 headless + 禁用写路由）？
