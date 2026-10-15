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
>
> **2026-10-15 可行性/跨平台补录**：新增 §5.1「3 条隐性硬前提」（单用户 IP 会话租约 → 机器人必须
> 同机、自启须带 data_dir、导证书校验）、§5.3「跨平台可用性」、§8 卡片更新次数上限与会话续租、
> §9 的 P0.5 验证阶段，并补全 §10 待确认项。
>
> **2026-10-15 并发模型补录**：新增 §5.4「部署场景与并发模型」（四种场景 × 必做项一张表），
> 含 C1/C2/C3 的据实重判（**C2 下调为非必做**、C1 可零改动规避、**C3 为唯一必做**）；
> §10「定位」标记为阻塞其它结论的前置项。

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
2. **webapi 默认绑 `0.0.0.0`**（对机器人而言这是**硬前提**，不只是"建议"）
   `settings_bind()` 默认值即 `"0.0.0.0"`（`webapi/mod.rs:225-236`）→ **默认对局域网开放**。
   个人隐私库这个默认偏激进；更要紧的是 webapi 有**单用户 IP 会话租约**（见下方「隐性硬前提 ①」），
   机器人**必须**与 webapi 同机走 `127.0.0.1`，不能走局域网 IP。
3. **token 从哪读**（不用改代码）
   `resolve_token()`（`webapi/mod.rs:148`）优先级 **env `LS_WEB_API_TOKEN` > `config.json` > DB >
   首次生成**，并始终回写 `config.json` → 机器人直接读该文件。
4. **"退出"是唯一死法**
   托盘「退出」→ `app.exit(0)` → webapi 与机器人一起死。可接受，但机器人侧要能**感知离线并告警**。

**另外 3 条隐性硬前提（不满足则方案 A 直接不可用）**：

| # | 硬前提 | 依据 / 后果 |
|---|---|---|
| ① | 机器人**必须与 webapi 同机**（走 `127.0.0.1`） | `webapi/session.rs:96-118` + `routes/mod.rs:66-69`：`session_guard` 挂在**整个** `/api/*`（token 之外的第二道锁），**同一时刻全网只允许一个活跃会话**，按来源 IP 判定。同机 → 与用户浏览器同 IP → `Renewed`，互不冲突 ✅；**异 IP 未超时 → 403**（`session.rs:214-228`），且**任何通过鉴权的请求都会续租**（`session.rs:109-112`）→ 机器人会**持续霸占**会话，把浏览器一直挡在 403（桌面端踢出后机器人立刻重新占用） |
| ② | 自启命令必须带上 data_dir | autostart 是 HKCU Run key，拉起时无参数 → 只会读 `config.json` 的默认 data_dir；用户若靠 `--data-dir` 指定库，自启会指向**错的库** |
| ③ | 机器人 HTTP 客户端要**跳过自签名证书校验** | webapi 是自签 TLS（`webapi/tls.rs`）；SAN 虽已含 `127.0.0.1`（`tls.rs:14-18`），但 CA 不受信 |

> 另有 1 条长连接期约束：机器人若订阅 `GET /api/events`（SSE）做进度卡片，**SSE 期间不续租**，
> 必须自己周期发 `POST /api/session/ping`，否则默认 **600s** 租约到期被 403（`session.rs:33,109-112`）。

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

### 5.3 跨平台可用性

**结论：核心链路（webapi + bot + `lark-cli`）三平台都能跑；
真正的跨平台短板在「交付与常驻形态」，不在功能本身。**

**① 应用本体：代码真跨平台，但交付只配了 Windows**

| 层 | 跨平台证据 | 交付状态 |
|---|---|---|
| Rust 应用 | 三平台 cfg 分叉齐全：`process.rs`（Win `CREATE_NO_WINDOW`／mac `path_helper`／Linux PATH 刷新）、`deps/commands.rs:117-148`（winget／brew／apt）、三套 OCR（mac Vision／win OCR／paddleocr） | ✅ 可编译 |
| 打包 | — | ⚠️ `tauri.conf.json:30` `"targets": ["msi"]` **只有 MSI**；`bundle.macOS.infoPlist` 已配但**无 dmg / deb / appimage**，且**无 CI**（无 `.github/workflows`） |

→ 即"跨平台可编译"≠"跨平台可交付"。要支持 macOS/Linux 装机，需补打包 target 与构建流程。

**② `lark-cli` 本体：官方支持三平台**

- package.json 声明 `os: [darwin, linux, win32]`、`cpu: [x64, arm64, riscv64]`
- 它是**原生二进制**（`bin/lark-cli.exe`，Windows 上 49 MB），`scripts/run.js` 只是 Node 薄壳
  → **可直连原生二进制，不必让用户装 Node**（对打包友好）
- 定位可执行文件可复用现成基础设施：`process.rs` 的 `new()` / `probe_ok()` / `windows_exe_name()`

**③ ⚠️ 一条跨平台共有的运维风险：`lark-cli` 会自更新**

`scripts/run.js` 里专门处理了 "crashed self-update left `.old`" → 说明存在自更新路径。
自更新会替换二进制（正在运行的 `consume` 可能受影响），且是**联网行为**，与内网/离线机器冲突。

**④ 方案 A 跨平台**

| 平台 | autostart | 托盘 | 结论 |
|---|---|---|---|
| **Windows** | HKCU Run key | ✅ | ✅ 参考实现 |
| macOS | LaunchAgent（`lib.rs:89` 已配 `MacosLauncher::LaunchAgent`） | ✅ | ⚠️ 需登录会话；关窗口已按 macOS 处理（`lib.rs:709-716`） |
| Linux | autostart `.desktop`，**需桌面会话** | 依赖 GTK 托盘 | ⚠️ 无桌面会话则整套不成立（退化成方案 B，而 B 在 Linux 又不可行） |

**⑤ 方案 B 跨平台**：见 §5.2 平台限制表（Windows 可、Linux 不可、macOS 与方案 A 差别不大）。

### 5.4 部署场景与并发模型

> 本节回答"定位不同，必做项差多少"。**先看两条决定一切的事实**。

**事实 1：多端登录不产生多客户端。**
`session_guard` 按**来源 IP** 识别身份（`webapi/session.rs:96-118`），机器人始终从本机 `127.0.0.1` 出去
→ 永远是**一个**客户端、一个会话租约。**§5.1 硬前提① 在任何"本机 bot"场景下自动满足**
（只有"bot + 远程浏览器**同时**用"才冲突，而那只发生在团队定位）。

**事实 2：飞书 p2p 会话是 per-user，不是 per-device。**
同一账号在手机/桌面/网页登录 → **同一个 `chat_id`** → 映射到**同一个 session**。
→ **多端登录不增加会话数，只增加触发源。**

**四种场景与必做项**：

| 场景 | webapi / 核心改动 | 必做的设计决策 | 主要风险 |
|---|---|---|---|
| **A. 个人助手 · 单端** | **0** | — | 仅 C3 |
| **B. 个人助手 · 多端同账号** | **0** | ① bot 会话空间是否独立 ② 长回答怎么适配手机 | 多端点击撞"最多更新 2 次"；C3 泄漏 → 重复投递**同时弹到所有端** |
| **C. 个人助手 · 多群** | **0** | 上面 + ③ **每会话串行队列 + 全局并发上限** | 真并发（N 群同问 → N 路抢网关配额） |
| **D. 团队 7×24**（多人各开 WebUI） | **必须改**：换掉 IP 租约鉴权 + per-session 取消 + 资源配额 | 多租户 / 权限 | **无头成为必需**——有头=桌面=单用户，D 与"不动无头"互斥 |

**三个具体技术点**：

1. **多端不增客户端**（见事实 1）——这是 §5.1 硬前提① 只在小范围成立的原因。
2. **桌面 UI 与 bot 会共写同一个 `chat_history.json`**
   bot 走 `/api/ai/conversation/ask/stream` 落库 → 与桌面 UI 是**同一个**会话存储。
   `webapi/session.rs:14-16` 的注释**已自认**："桌面端与 Web 端仍可能同时操作，因此聊天历史仍有锁"。
   bot 于是成为**第三个写者**，而 `AGENTS.md` 的 #3（会话丢失）、#4（内容不落库）**都是这个存储的并发问题**。
   → **决策**：bot 用独立命名空间（如 `lark:p2p:<open_id>` / `lark:chat:<chat_id>`）还是共用？
3. **卡片更新预算怕多端点击**
   `card.action.trigger` 的 token「30 分钟、**最多更新 2 次**」；手机点一次、桌面再点一次 → 瞬间用满。
   → 按钮回调必须**幂等** + 预留更新预算。

**并发与生命周期的当前判定**：

| # | 问题 | 判定 | 依据 |
|---|---|---|---|
| **C1** | 多会话并发（取消串扰） | 场景 A/B：**靠排队完全规避，0 核心改动**；场景 C：**必须做队列** | `AI_CANCEL` 是全局 `AtomicBool`（`ai/mod.rs:1567`）且**无 single-flight 守卫**；每次生成都 `reset_ai_cancel()`（`commands/ai.rs:372`）→ 并发时会互清标志。桌面端靠 **UI 单飞**兜住 |
| **C2** | 索引期资源竞争 | **已被削平，非必做**（曾定为必做，据实下调） | ① `abb4583` 已在 HEAD（提取期不再持连接）② AI 路径本就是"借→查→还→再生成"（`ai/prompt.rs:65`、`ai.rs:214`）③ 已有交互优先机制 `ChatTurnGuard`（`ai.rs:375`）。剩"变慢"，bot 侧超时+退避足够 |
| **C3** | 进程生命周期 / 优雅停机 / daemon 归属 | **唯一必做**（约两三百行） | Windows 无可用 `SIGTERM`，只能靠**关 stdin** 优雅停；event bus daemon 是**跨 app 单例**、归属未定义；处理不当 → 订阅泄漏 → 重复投递，属"**静默坏**"类（对照 `AGENTS.md` 4 条历史教训） |

> 另一条飞书侧硬约束：**同一 app 只能有一个 event bus 连接**（`another bus already connected` → 校验失败 exit 2）
> → "单机器人"被强制，**不能靠多开 consumer 扩并发**，并发只能在一个进程内解决。

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

## 八、另外几个必须处理的技术点

| 问题 | 说明 | 做法 |
|---|---|---|
| **慢回答的体验** | RAG 要几秒到几十秒；飞书有消息长度、卡片更新次数、QPS 限制 | 先回"处理中"卡片 → 用 patch 逐步更新。**能用的 SSE 只有两条**：`POST /api/ai/conversation/ask/stream`（按 session 推流）与全局 `GET /api/events`（含 `ai-chunk`/`ai-progress`/`ai-done`）。`/api/ai/events` 是普通 GET，只能轮询已落库事件 |
| **幂等** | 事件可能重投（官方文档明确提到订阅泄漏会导致重复投递） | 按 `message_id` 去重，否则重复回答 |
| **离线感知** | GUI 退出/重启时 webapi 不可用，机器人会"静默失联" | 健康检查 + 向指定群告警 |
| **卡片更新次数上限** | 卡住 P1 的真实限制 | `card.action.trigger` 的 token **有效期 30 分钟、最多更新 2 次**（`lark-cli event schema card.action.trigger` 原文）；飞书对单条消息的卡片更新次数另有限制。**不能每个 SSE chunk 都 patch 一次** → 必须**节流合并**（按时间或 token 数批量更新），否则必然超限 |
| **会话续租** | 机器人是长时间常驻进程 | 见 §5.1「隐性硬前提」：同机 `127.0.0.1` + 周期 `POST /api/session/ping`，否则 600s 后被 403 |

---

## 九、分阶段实施建议

| 阶段 | 内容 | 前置 |
|---|---|---|
| **P0** | `lark-cli config init` 建自建应用并授权；`event consume` 拿到 `im.message.receive_v1` 样本，确认字段 | 管理员审批 scopes **+ 控制台勾开事件**（`im.message.receive_v1` 属 "Required Console Events"，非订阅即用） |
| **P0.5**（验证地基，1~2 天，不动代码） | 打掉三个不确定项：<br>① `event consume im.message.receive_v1 --dry-run` 确认身份（`--as bot/user`）与前置<br>② 同机 `curl` 实测 webapi 单会话租约对机器人是否友好（§5.1 硬前提 ①）<br>③ `event consume card.action.trigger` 确认 **`callback` 类型**在长连接下可消费 | P0 |
| **P1** | 白名单群里 @机器人提问 → `POST /api/ai/conversation/ask/stream`（SSE）→ **节流更新**卡片；**autostart 开关**补上（带 data_dir）；只回摘要 | 数据边界规则定好 |
| **P2** | 文件 / 语音 / 图片入站（下载 → 监控目录 → 自动入库 → 回执） | P1 稳定 |
| **P3** | 定时摘要 + 出口（写飞书文档/表格）+ 卡片回调 | P0.5 ③ 已验证 `card.action.trigger` 可用 |
| **P4**（可选） | headless `serve` 模式（方案 B），用于无 GUI 的机器 | 明确目标机器平台；**先做 Windows 无窗口 spike** |

---

## 十、待确认事项

- [ ] **定位（阻塞其它所有结论的前置项）**：个人助手（方案 A）还是团队 7×24 服务？→ 不勾这条，§5.4 的必做项无法收敛
- [ ] **目标机器**：Windows 桌面常驻 / Windows 服务 / Linux NAS？（Linux → 方案 B 不成立）
- [ ] **数据边界**：哪些目录可外发？是否只回摘要？是否要脱敏闸门？
- [ ] **身份**：`--as bot` 还是 `--as user`（后者权限更大更敏感）？
- [ ] **访问控制**：哪些群 / 哪些人可用？
- [ ] 是否接受"GUI 与 headless 互斥"，还是要求共存（需只读 headless + 禁用写路由）？
- [ ] **跨平台范围**：只发 Windows（现状只有 MSI），还是也要 macOS/Linux 装机？（要后者需补打包 target 与 CI）
- [ ] **机器人部署位置**：确认与 webapi **同机**（`127.0.0.1`）？（异机 → 单会话租约会互踢，见 §5.1 硬前提 ①）
- [ ] **`lark-cli` 的取得方式**：随包分发（+49 MB，需按 target triple 打包）还是要求用户自装？（注意其**自更新**行为，§5.3 ③）
- [ ] **`card.action.trigger` 的长连接支持**：P3 卡片回调的唯一技术不确定项，建议 P0.5 先验
