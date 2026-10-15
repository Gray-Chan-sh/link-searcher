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
>
> **2026-10-15 三端交互重构评估**：新增 §5.5（阶段化计划、耦合面、风险 R1–R9、决策 D1–D5）。
> **仅阶段一获批**；阶段二/三/四待逐段确认。
>
> **2026-10-15 配置写入补录**：新增 §5.6（原子性 / 覆盖 / 多写者三层 + A/B 两档）。
> **D1 已定＝放弃 IP 排他**（阶段四解锁）；配置写入的原子化已实施。
>
> **2026-10-15 P0 实测入档**：§2 新增「P0 实测记录」——应用 `cli_aa5a2e242e785c09` 已接通、
> 长连接契约与真实事件样本逐字段确认（`content` 已预渲染、去重键为 `message_id`）、
> **收 + 发闭环已验证**；§9 的 P0 标记完成、P0.5 仅剩卡片回调一项待验。
>
> **2026-10-15 P0.5 完成**：`card.action.trigger` **确认可走长连接**；§2 补入卡片回调样本与
> 4 条实测发现（`action_value` 需二次 parse、`card_content` 被重写、两个 EventKey 的 `event_id`
> 语义不同、`timestamp` 精度不一致）。**技术不确定项清单已清空**，后续为纯实现。

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

**当前状态**：✅ **已接通**（2026-10-15，App ID `cli_aa5a2e242e785c09`）。
`lark-cli config init --new` 的浏览器流程**一次到位**，**无需**再手工去控制台勾机器人能力/事件/scope。

### P0 实测记录（2026-10-15）

**配置与授权**

| 检查 | 结果 |
|---|---|
| `lark-cli auth status` | appId `cli_aa5a2e242e785c09`、brand `feishu`、**bot identity `ready`**、`tokenStatus: ready` |
| `lark-cli doctor` | `config_file` / `app_resolved` / `bot_identity` / `identity_ready` / `endpoint_open` / `endpoint_mcp` **全 pass**；仅两条 warn：`cli_update`（1.0.96→1.0.98）、`user_identity`（未登录，本次不需要） |
| `event consume … --dry-run` | `status: ready`；前置 `credentials_available` / **`console_event_published`** / **`scopes_granted`** **全 ok** |

> ⚠️ **更正一个早先的担心**：原以为浏览器授权之外还要手工去控制台补"事件订阅 / scope"。实测**不需要**——
> `console_event_published: ok` + `scopes_granted: ok` 说明授权流程已一并完成。

**长连接子进程契约（实证）**

```
[event] started bus daemon pid=15700 (auto-exits 30s after last consumer)
[event] to stop gracefully: send SIGTERM (kill <pid>). Avoid kill -9 — it skips cleanup and may leak server-side subscriptions.
[event] ready event_key=im.message.receive_v1
[source] feishu-websocket: connected
[event] exited — received 1 event(s) in 4m11s (reason: limit)
```

- ✅ 就绪标记 `[event] ready event_key=<key>` 原文确认
- ✅ **官方自己**就提示"不要 `kill -9`"（§2 那条不是我推的）
- ✅ 边界退出：`--max-events` 触发 → `reason: limit` = **正常退出（0）**；daemon 在最后一个消费者退出后
  **30s 自清**，**无残留订阅**
- ✅ **stdout / stderr 必须分流**：诊断（`[event] …`）走 **stderr**，事件 JSON 走 **stdout**
  → bot 应**阻塞读 stderr 等到就绪标记，然后只解析 stdout**（逐行 NDJSON）

**真实事件样本**（注意 `content` 已是**预渲染的裸文本**）

```json
{"type":"im.message.receive_v1","event_id":"8a912373…","timestamp":"1792054128477",
 "id":"om_x100b623d7830c4a8c093ff978ce62a0","message_id":"om_x100b623d7830c4a8c093ff978ce62a0",
 "create_time":"1792054128004","chat_id":"oc_be9fcfffcb9b1b46349fc347c608914b","chat_type":"p2p",
 "message_type":"text","sender_id":"ou_bfdf6618eacf1f52eaa1911ca71ace6a","sender_type":"user",
 "content":"hello"}
```

| 字段 | 实测 | 结论 |
|---|---|---|
| `content` | `hello`（**裸文本，不是 `{"text":"hello"}` 信封**） | ✅ schema 所称 "pre-rendered human-readable text" 证实 → **bot 省掉一层解信封代码**，`image/file/audio` 同受益 |
| `message_id` / `id` | 两者**完全相同** | ✅ `id` 是 legacy alias → 统一用 `message_id` |
| `event_id` | 与 `message_id` **不同** | ⚠️ 官方明确**不可**用作去重键（重投会变）→ §8「幂等」用 `message_id` **获得证实** |
| `chat_id` / `chat_type` | `oc_…` / `p2p` | ✅ `p2p` 是 **per-user**（§5.4 事实 2 证实）；`oc_…` 即 §5.5 阶段三要持久化的**映射键** |
| `sender_id` | `ou_…` | ✅ `open_id` 形态 → §7 用户白名单可直接以它为键 |
| `timestamp` vs `create_time` | 投递比创建晚 473ms | 两者语义不同，勿混用 |

**回程亦已验证**：`lark-cli im +messages-reply --message-id <收到的> --text …` 返回
`{"ok":true,"identity":"bot","data":{"message_id":"om_…"}}`，飞书端可见 → **收 + 发闭环成立**。

**卡片回调（P0.5 ③）✅ 长连接可行** —— `card.action.trigger` 的 `subscription_type` 是 **`callback`**，
与 `im.message.receive_v1` 的 `event` 是**两类订阅**：浏览器授权流程只覆盖了后者，前者需**单独订阅**
（`event consume … --dry-run` 会给出 `console_event_published: blocked` + 一条订阅链接）。订阅后实测样本：

```json
{"type":"card.action.trigger","event_id":"9baa361ac0deeb30ce7d9d8da075ed7f","timestamp":"1792054882978640",
 "operator_id":"ou_…","message_id":"om_x100b623d2be07878ce8b7fa990baf21","chat_id":"oc_…","host":"im_message",
 "token":"c-fd61f1c5…","action_tag":"button","action_value":"{\"probe\":\"card-action-20261015\"}",
 "checked":false,"card_content":"{\"config\":{},\"elements\":[…,{\"actions\":[{\"behaviors\":[{\"type\":\"callback\"}],
 \"custom_action_id\":\"act_v1_7696817043099012085\",…}],\"tag\":\"action\"}],\"header\":{…}}"}
```

四个**只有实测才知道**的点：

| # | 发现 | 影响 |
|---|---|---|
| ① | `action_value` 是**字符串形式的 JSON**（`"{\"probe\":…}"`），不是对象 | bot 必须**二次 parse** 才能拿到自己埋的路由信息 |
| ② | **`card_content` 被飞书重写、非原文回显**：`config.wide_screen_mode` 丢失；按钮 `value` 变成 `behaviors:[{type:callback}]` + **`custom_action_id`**；`text` 多出 `i18n` | **不能**靠"比对 `card_content` 与发送原文"判断卡片状态；路由要靠 `action_value` / `custom_action_id` |
| ③ | 两个 EventKey 的 `event_id` **语义不同** | 消息：schema 明言 "delivery ID，**不可**用于去重"→ 用 `message_id`；卡片：schema 写作 "**Globally unique** event ID" → 可作去重键。**去重键不能一把抓** |
| ④ | `timestamp` **精度不一致** | 消息事件是**毫秒**（13 位）、卡片事件是**微秒**（16 位）→ 不能按同一精度解析 |

另：`token` 实测存在（即 §8「30 分钟 / 最多更新 2 次」的落点）；`host: "im_message"` 为 schema 未见字段。

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

> **实证（2026-10-15）**：`doctor` 与几乎每条命令的 `_notice` 都带
> `lark-cli 1.0.98 available, current 1.0.96, run: lark-cli update` → 自更新通道确实存在。
> 建议**不要在接入过程中顺手升级**：它会替换二进制，而 P0 的全部验证都是在 1.0.96 上做的。
> 要升就单独升、并重跑一遍 §2 的实测记录。

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

### 5.5 三端交互重构：评估与分阶段计划

> **状态**：评估已确认，**阶段一**获批实施；阶段二/三/四待逐段确认。本节记录评估结论，避免重复讨论。
> 背景：三端（UI 走 IPC、Web/飞书走 webapi）共用两处**排他资源**——AI 取消标志与会话租约。

**目标形态**：会话成为划分单元——AI 生成按会话隔离；会话按端（`origin`）隔离；两者由一个统一管理模块收敛。

**已定决策**：

| 编号 | 决策 |
|---|---|
| **D2** | **取消扫描不再连带取消 AI**（`webapi/routes/index.rs:243` 解耦） |
| **D3** | 旧的无参取消请求 = **取消全部在途生成**（过渡兼容，前端升级后移除） |
| **D4** | 注册表落在**新建 `commands/sessions/`**，同时承担来源映射与 in-flight 仲裁 |
| **D5** | 删除正在生成的会话 → **先取消该会话生成、待结束后再删**；保存时若会话已不存在则丢弃结果 |
| **D1** | ✅ **已定（2026-10-15）：放弃 IP 排他** → 阶段四解锁（`session_guard` 移除或降级为"仅展示在线状态"）。代价：token 成为唯一门槛、桌面"强制踢出"语义消失；`GET /api/session`、`/ping`、`/logout` 与前端 `session-denied` 遮罩随之失去意义 |

**耦合面（已逐条核实）**：

| 资源 | 实现 | 耦合点 |
|---|---|---|
| AI 取消 | 进程级 `static AI_CANCEL`（`ai/mod.rs:1567`） | 读 7 处（`ai/mod.rs:1372,1430`；`commands/ai.rs:384,390,546,552`；`commands/ai/prompt.rs:227`）；清零 4 处（`commands/ai.rs:372,420,528,593`）；置位 3 处（`commands/ai.rs:17`、`routes/ai.rs:478`、`routes/index.rs:243`） |
| 会话存储 | 单文件 `chat_history.json` + `history_lock` + `mutate_history`（`commands/ai/session.rs:128-151`） | 生产与测试约 20 处直接读写；`routes/ai.rs:102,134` |
| 会话租约 | `session_guard` 挂整个 router（`routes/mod.rs:66-69`） | `ClientIp`；`session::clear` 于 token 轮换（`routes/settings.rs:114`、`commands/settings.rs:93`）与桌面踢出（`commands/webapi.rs:23`） |
| 测试 | — | `tests/webapi_http.rs` 断言租约/`is_owner`/ping/logout；`api/__tests__/sessionClient.test.ts` 断言 403→`session-denied` |
| 前端 | — | `api/files.ts:143` 与 `api/client.ts:149` 的取消**无参数**；`client.ts:263,302,351` 的 `session-denied`/`auth-failed` |

**风险清单**：

| # | 风险 | 级别 | 缓解 |
|---|---|---|---|
| R1 | 删除会话后在途生成把它**复活** | 高 | `save_chat_session_impl` 的 `None` 分支会 `push`（`session.rs:332`），前端 `streamStore.ts:53` 还会自行落库 → 按 D5 先取消、保存时校验存在性 |
| R2 | 放弃 IP 排他 = **安全模型变化** | 高 | 仅阶段四；必须显式写入文档，不默默丢失 |
| R3 | `cancel_scan` 与 `cancel_ai` 耦合 | 中 | 按 D2 解耦 |
| R4 | bot 后台写 + 整会话覆盖（`*existing = session`） | 中 | 隔离后**禁止跨 origin 写**；统一模块第一版**只读**聚合 |
| R5 | 单文件全量读写 | 中（既有） | bot 接入后调用量上升会放大 → 上线前实测会话规模与耗时 |
| R6 | 取消令牌生命周期竞态 | 中 | 注册表用 RAII（完成即摘除）；取消幂等 |
| R7 | 旧会话来源不可判定 | 低 | 统一归 `ui` 并在文档说明 |
| R8 | 前端取消协议改变 | 中 | 按 D3 保留无参兼容 |
| R9 | **无"取消生效"端到端测试** | 中 | 阶段一必须补，否则回归无法发现 |

**分阶段计划**：

| 阶段 | 内容 | 前置 / 验收 |
|---|---|---|
| **一（已获批）** | AI 取消按会话：in-flight 注册表 + 令牌；`/api/ai/cancel` 带 `session_id`；`cancel_scan` 解耦 | 并能回滚到静态 flag。验收：A/B 两会话并发生成，取消 A 不影响 B；取消晚于完成为无操作；删除中生成被中止且不复活 |
| 二 | `ChatSession.origin`（`#[serde(default)]`，向后兼容）+ 列表过滤 + 写入限同 origin | 旧文件原样加载；三端各自只见己方会话；统一视图只读 |
| 三 | 飞书 `chat_id ↔ session_id` 映射（独立文件）| 需先完成 §5.4 的 **C3 生命周期**；验收：bot 重启映射保留、重复事件不产生重复会话 |
| 四 | 租约重构（`session_guard` 移除/降级）| **D1 已定＝放弃 IP 排他**；验收：手机浏览器与 bot 可同时使用 |

> 注：**UI 端不走 webapi**，故阶段四对它无影响；三端中只有 Web↔飞书受租约约束。同理，
> 现有"单写入者"是**不完整**的保证——`webapi/session.rs:14-16` 注释已自认"桌面端与 Web 端仍可能
> 同时操作"，且破坏性操作另有 CAS（`state.rs:188-191`）。

### 5.6 配置写入：原子性 / 覆盖 / 多写者

> 起因：讨论"配置能不能全部放进配置文件（像 OpenClaw 那样）"时暴露的问题。OpenClaw 用**单一**
> `~/.openclaw/openclaw.json`（JSON5），但它是**围绕这个文件配了一整套机制**才敢这么做
> （原子写、严格校验、热重载、last-known-good、意外覆盖防护、secret refs）。本节把结论分层记下。

**三类故障彼此独立，解法不可互换**

| 故障 | 谁造成 | 解法 | 现状 |
|---|---|---|---|
| **写到一半**（半截 / 0 字节） | **任何**写入方（含我们自己的代码） | **原子写**（tmp → fsync → rename） | ✅ 本轮已修 |
| **我改了、被应用覆盖** | 多个写入方：手改 vs 应用整文件重写 | **文件监听（热重载）+ 意外覆盖防护** | ❌ 未做 |
| **两进程同时写、互相覆盖** | 多进程（CLI / headless / bot sidecar） | **文件锁** | ❌ 未做（`CONFIG_LOCK` 只是**进程内** Mutex） |

⚠️ 三者不可互相替代：**原子的写照样能吞掉另一个原子的写**；热重载也不防"写到一半"。

**A / B 两档（决定"允不允许手改"）**

| 档 | 内容 | 需要做 |
|---|---|---|
| **A** | 配置**只由应用写**（UI / CLI / API 都走同一个原子写函数），不承诺手改 | 原子写 + 未知键告警（**本轮已完成**） |
| **B** | **配置文件为中心、允许手改**（＝ OpenClaw 形态） | A 档 **再加**：文件监听热重载 + 意外覆盖防护（+ 可选 `$include` / secret refs） |

**本轮已修（`src-tauri/src/config.rs`）**

1. **原子写** `write_config_atomic`：同目录临时文件（**带 pid**，避免跨进程撞名互相撕碎）
   → unix `0600` → `sync_all` → `rename`（Windows 退避重试 ×5）→ POSIX 目录 fsync。
   全部写入路径（`save_config` / `save_embed_plan` / `set_web_api_token` / 启动迁移）收口于此。
   参考先例：`commands/ai/session.rs::write_history_unlocked`。
2. **损坏配置不再被销毁**：解析失败 → 另存 `config.json.corrupt.<unix_ts>` + 以默认值继续
   （旧实现是**当场把默认值写回**，等于把唯一现场销毁）；读取失败 / 另存失败 → **一个字都不写**，
   原文件原样保留。
3. **未知键告警**（而非拒绝）：`KNOWN_CONFIG_KEYS` 与 `AppConfig` 字段由一个测试守着同步。

**为什么没有用 `#[serde(deny_unknown_fields)]`**（刻意偏离最初的提议）：它会让**一个拼错的键
导致整份配置被拒** → 走进上面"另存 + 默认值"的路径，用户看到的是"配置全没了"；而当前
**没有 `doctor --fix` 之类的修复入口**。此外 `commands/backup.rs` 的测试用 `{"theme":"dark"}`
当 config fixture，硬拒绝会连带把那些测试打红。等有了修复路径再切硬拒绝。

**仍待处理（B 档的前置）**：`embed_plans` 是**运行期高频写**的字段，却待在会被整文件重写的
`config.json` 里；`web_api_token` 仍**双存储**（config.json + DB `app_settings`，靠
`resolve_token()` 的 env > config.json > DB 优先级对齐）。若走 B 档，这两处应一并重新分工。

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
| **卡片回调**：按钮"追问/反馈/打开详情"（`card` 域事件）✅ **2026-10-15 实测可行**（callback 类型可走长连接，见 §2） | ⭐⭐ |

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
5. **凭据存储**：app_secret **不得进我们的 `config.json`**（它已是明文，且装着 AI `api_key`）。
   实测（2026-10-15）由 `lark-cli` **自持**：Windows 下位于 `%USERPROFILE%\.lark-cli\config.json`
   （`keychain-downgrade` 仅 macOS 有意义）；`lark-cli config show` 对 secret 显示 `****`。
   我们的进程**零接触、不复制**。⚠️ 因此**备份/打包时不要把该文件带上**，它不在 `data_dir` 里，
   容易被"顺手一起备份"带走。

> **这一条不解决，不建议做。**

---

## 八、另外几个必须处理的技术点

| 问题 | 说明 | 做法 |
|---|---|---|
| **慢回答的体验** | RAG 要几秒到几十秒；飞书有消息长度、卡片更新次数、QPS 限制 | 先回"处理中"卡片 → 用 patch 逐步更新。**能用的 SSE 只有两条**：`POST /api/ai/conversation/ask/stream`（按 session 推流）与全局 `GET /api/events`（含 `ai-chunk`/`ai-progress`/`ai-done`）。`/api/ai/events` 是普通 GET，只能轮询已落库事件 |
| **幂等** | 事件可能重投（官方文档明确提到订阅泄漏会导致重复投递） | 按 `message_id` 去重，否则重复回答。**2026-10-15 实测证实**：`id` 与 `message_id` 相同（前者是 legacy alias），而 `event_id` 不同、且官方明言**不得**用作去重键 |
| **离线感知** | GUI 退出/重启时 webapi 不可用，机器人会"静默失联" | 健康检查 + 向指定群告警 |
| **卡片更新次数上限** | 卡住 P1 的真实限制 | `card.action.trigger` 的 token **有效期 30 分钟、最多更新 2 次**（`lark-cli event schema card.action.trigger` 原文；**2026-10-15 实测回调里确实带 `token`**）；飞书对单条消息的卡片更新次数另有限制。**不能每个 SSE chunk 都 patch 一次** → 必须**节流合并**（按时间或 token 数批量更新），否则必然超限。另：`card_content` 会被飞书**重写**（非原文回显，见 §2 实测发现 ②） |
| **会话续租** | 机器人是长时间常驻进程 | 见 §5.1「隐性硬前提」：同机 `127.0.0.1` + 周期 `POST /api/session/ping`，否则 600s 后被 403 |

---

## 九、分阶段实施建议

| 阶段 | 内容 | 前置 |
|---|---|---|
| **P0** ✅ **完成（2026-10-15）** | `config init --new` 建应用并授权（App ID `cli_aa5a2e242e785c09`）；已拿到真实样本并逐字段确认；**收 + 发闭环已验证** —— 全部记录见 §2「P0 实测记录」 | 前置由浏览器授权流程**一并满足**，无需手工补控制台事件/scope |
| **P0.5** ✅ **完成（2026-10-15）** | ① ✅ 身份实测为 **`bot`**（`defaultAs: auto` 解析到 bot）<br>② ~~同机单会话租约实测~~ **已作废**（D1 已定＝放弃 IP 排他，租约将在阶段四拆除）<br>③ ✅ **`card.action.trigger` 的 `callback` 类型确认可走长连接**（样本与 4 条实测发现见 §2） | P0 ✅ |
| **P1** | 白名单群里 @机器人提问 → `POST /api/ai/conversation/ask/stream`（SSE）→ **节流更新**卡片；**autostart 开关**补上（带 data_dir）；只回摘要 | 数据边界规则定好 |
| **P2** | 文件 / 语音 / 图片入站（下载 → 监控目录 → 自动入库 → 回执） | P1 稳定 |
| **P3** | 定时摘要 + 出口（写飞书文档/表格）+ 卡片回调 | P0.5 ③ 已验证 `card.action.trigger` 可用 |
| **P4**（可选） | headless `serve` 模式（方案 B），用于无 GUI 的机器 | 明确目标机器平台；**先做 Windows 无窗口 spike** |

---

## 十、待确认事项

- [ ] **定位（阻塞其它所有结论的前置项）**：个人助手（方案 A）还是团队 7×24 服务？→ 不勾这条，§5.4 的必做项无法收敛
- [ ] **目标机器**：Windows 桌面常驻 / Windows 服务 / Linux NAS？（Linux → 方案 B 不成立）
- [ ] **数据边界**：哪些目录可外发？是否只回摘要？是否要脱敏闸门？
- [x] **身份** ✅：实测为 **`bot`**（`defaultAs: auto` 解析到 bot）；user 身份未登录、本次也不需要
- [ ] **访问控制**：哪些群 / 哪些人可用？
- [ ] 是否接受"GUI 与 headless 互斥"，还是要求共存（需只读 headless + 禁用写路由）？
- [ ] **跨平台范围**：只发 Windows（现状只有 MSI），还是也要 macOS/Linux 装机？（要后者需补打包 target 与 CI）
- [ ] **机器人部署位置**：确认与 webapi **同机**（`127.0.0.1`）？（异机 → 单会话租约会互踢，见 §5.1 硬前提 ①）
- [ ] **`lark-cli` 的取得方式**：随包分发（+49 MB，需按 target triple 打包）还是要求用户自装？（注意其**自更新**行为，§5.3 ③）
- [ ] **`card.action.trigger` 的长连接支持**：P3 卡片回调的唯一技术不确定项，建议 P0.5 先验
