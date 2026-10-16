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
> **D1 已定＝放弃 IP 排他**（阶段四**已实施**：`session_guard` 降级为仅记录在线状态）；配置写入的原子化已实施。
>
> **2026-10-15 P0 实测入档**：§2 新增「P0 实测记录」——应用 `cli_aa5a2e242e785c09` 已接通、
> 长连接契约与真实事件样本逐字段确认（`content` 已预渲染、去重键为 `message_id`）、
> **收 + 发闭环已验证**；§9 的 P0 标记完成、P0.5 仅剩卡片回调一项待验。
>
> **2026-10-15 P0.5 完成**：`card.action.trigger` **确认可走长连接**；§2 补入卡片回调样本与
> 4 条实测发现（`action_value` 需二次 parse、`card_content` 被重写、两个 EventKey 的 `event_id`
> 语义不同、`timestamp` 精度不一致）。**技术不确定项清单已清空**，后续为纯实现。
>
> **2026-10-15 §7/§10 收口**：安全与数据边界**已定**（全部可外发、放弃闸门 1–3、保留空收紧阀；
> 访问控制＝token + **仅私聊不得进群**，附 3 项落地要求）；§10 绝大多数已定并勾选，
> §5.2 方案 B 标注**不在范围内**。
>
> **2026-10-15 §7.1 访问控制规格**：把访问控制升级为**用户白名单（fail-closed）+ 仅私聊**，
> 并写成可实现规格（判定顺序 / 空值语义 / 提示策略 / 冷启动配对 / 测试要求 / 实测真实值）。
> 特别固化"**两个空数组方向相反**"（访问控制 fail-closed vs 外发阀 fail-open）。

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

`webapi/mod.rs:33-51` 定义 `BRIDGED_EVENTS`，把 Tauri 事件转发给 Web 客户端（SSE）：

```
scan-progress / scan-completed / ai-chunk / ai-progress / ai-done
migration-progress / migration-warning / funasr-install-done / bge-install-done
restore-completed / dep-progress / dep-install-done
config-changed
```

机器人可以用 `/api/events`（SSE）拿进度，做"处理中"卡片。

> ⚠️ **这张表是白名单，漏一条的后果只在浏览器里出现**（桌面走 Tauri IPC，照发照收）。
> `config-changed` 曾是漏的（见 §5.9 真机验收 · 缺陷①），现在由 `webapi/mod.rs` 的
> `bridged_events_cover_frontend_subscriptions` 测试守住：**前端订阅的事件必须在这张表里**
> （确实桌面专属的则要登记进测试里的 `TAURI_ONLY_EVENTS` 并写明理由）。

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

1. ~~**autostart 没接线**~~ ✅ **已接线（2026-10-15）**
   插件已装、权限已授（`autostart:default`）；新增 Tauri 命令 `autostart_state` / `set_autostart`
   走插件 **Rust API**，设置页开关改为**以操作系统状态为准**（原先那个开关只写设置、无人读）。
   ⚠️ **data_dir 的坑**：插件 API **不接受参数**，自启是**无参数**启动 → 读的是 `config.json` 里的
   `data_dir`。若用户以 `--data-dir` 启动，自启会指向**另一个库** → 命令会回 `warning` 让 UI 显式提示
   （而不是默默错开）。本机 data_dir 存在 config.json 里，故无此提示。
   ⚠️ **dev/install 差异（实测）**：Run 项记的是**当时启动它的那个 exe 路径**。在 `tauri dev` 里点
   开关 → 记的是 `target/debug/link-searcher.exe`（**dev 版**）；开机自启会拉起 dev 程序，而它的前端
   指向 `devUrl: 127.0.0.1:1420` —— **vite 没在跑时窗口是空白/报错页**（Rust 侧与 bot 仍正常启动）。
   → ⚠️ **切到安装版时它通常已经是 ON**（只是指向 dev 的 exe）→ 正确操作是「**先关一次、再开一次**」，
   否则会把它点成 OFF（2026-10-15/16 实测踩到）。复核方法：`Get-ItemProperty 'HKCU:\...\Run'` 看值是否指向安装路径。
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
| ① | 机器人**必须与 webapi 同机**（走 `127.0.0.1`） | `webapi/session.rs:96-118` + `routes/mod.rs:66-69`：`session_guard` 挂在**整个** `/api/*`（token 之外的第二道锁），**同一时刻全网只允许一个活跃会话**，按来源 IP 判定。同机 → 与用户浏览器同 IP → `Renewed`，互不冲突 ✅；**异 IP 未超时 → 403**（`session.rs:214-228`），且**任何通过鉴权的请求都会续租**（`session.rs:109-112`）→ 机器人会**持续霸占**会话，把浏览器一直挡在 403（桌面端踢出后机器人立刻重新占用）**✅ 已解决（2026-10-15 阶段四）**：`session_guard` 已**降级为"仅记录在线状态"**（`GuardOutcome::Denied` 已删除，不再有任何 403）→ **手机浏览器与 bot 可同时使用** |
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

> **2026-10-15：不在范围内。** 定位已确定为「个人助手 · 桌面常驻」（§十），headless `serve` 不实施；
> 本节仅作技术记录（含平台限制与 data_dir 互斥的硬约束，将来若重启该方向可直接复用）。

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

**① 应用本体：代码真跨平台；打包 target 已补，但仍未在 mac/Linux 实测**

| 层 | 跨平台证据 | 交付状态 |
|---|---|---|
| Rust 应用 | 三平台 cfg 分叉齐全：`process.rs`（Win `CREATE_NO_WINDOW`／mac `path_helper`／Linux PATH 刷新）、`deps/commands.rs:117-148`（winget／brew／apt）、三套 OCR（mac Vision／win OCR／paddleocr） | ✅ 可编译 |
| 打包 | 平台专属配置：Tauri v2 支持 `tauri.<platform>.conf.json`，按 **JSON Merge Patch** 与主配置合并（**数组是整体替换**，不是追加） | ✅ **已补（2026-10-16）**：base `bundle.targets = "all"`（＝该平台的默认目标集合），新增 `src-tauri/tauri.windows.conf.json` 把 Windows **钉回 `["msi"]`** ⇒ **Windows 产物与本轮之前完全一致**；macOS ⇒ `app`+`dmg`；Linux ⇒ `deb`+`rpm`+`appimage`。sidecar 命名本来就按宿主 triple 生成（`scripts/build-sidecar.mjs` 用 `rustc -vV` 取 host triple，仅 Windows 加 `.exe`）——**无需改** |
| 构建流程 | — | ⚠️ 仍**无 CI**（无 `.github/workflows`）、无一键出多平台包 |

→ 即"跨平台可编译"≠"跨平台可交付"。本轮只补齐了**配置层**：Windows 侧用真实
`npm run tauri:build` 验过（照旧产出 MSI，说明平台专属配置的合并生效）；
**mac/Linux 从未被打包实测**（本机是 Windows，跨平台打包需要对应机器）——
不要把这一行读成"mac/Linux 交付已验证"。

**② `lark-cli` 本体：官方支持三平台**

- package.json 声明 `os: [darwin, linux, win32]`、`cpu: [x64, arm64, riscv64]`
- 它是**原生二进制**（`bin/lark-cli.exe`，Windows 上 49 MB），`scripts/run.js` 只是 Node 薄壳
  → **可直连原生二进制，不必让用户装 Node**（对打包友好）
- 定位可执行文件可复用现成基础设施：`process.rs` 的 `new()` / `probe_ok()` / `windows_exe_name()`
- **✅ 已修（2026-10-16，C7）：npm 前缀不再只认 Windows**。原先只硬编码了
  `%LOCALAPPDATA%\Programs\nodejs\node_modules`（本机布局），mac/Linux 上等于**只能靠 PATH 碰运气**。
  现改为**反推前缀**（`deps/catalog.rs::npm_node_modules_roots`）：`NPM_CONFIG_PREFIX` ／ `NVM_DIR` ／
  **PATH 上 node/npm 所在目录** → `bin_dir_roots`（`…/bin/node` ⇒ `…/lib/node_modules`，即
  npm 在 Unix 的默认布局；另含 `…/node_modules` 与 `node.exe` 同级两种 Windows 布局）。
  Windows 另补 `%APPDATA%\npm`（官方安装器的 npm 默认前缀）。纯路径推导（`bin_dir_roots`）有单测。
  > Unix 上本来就能工作（npm 的全局 `bin` 目录在 PATH 上，裸名 `lark-cli` 即可解析），
  > 但这个"靠 PATH 碰运气"并不是**保证** —— 现在它与 Windows 走同一条显式解析。

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
| **C3** | 进程生命周期 / 优雅停机 / daemon 归属 | ✅ **核心已实现并实测（2026-10-15）**：GUI 在 `setup` 里拉起 bot（`lark_bot.rs`，按**与主程序同目录**解析可执行文件 —— dev 直接用 `target/debug/lark-bot.exe`，装包后由 `externalBin` 放到同目录；两种命名都认，见 §5.7）、`RunEvent::Exit` 里收掉它。**两个实测发现**：① **应用被硬杀/崩溃时 `RunEvent::Exit` 不会执行 → 会留下孤儿 bot**（实测确认；危害是它继续占长连接，且应用重启后会**同时跑两个 bot → 重复回答**）→ 已给 bot 加 `--parent-pid` 看门狗（`sysinfo` 轮询，父进程消失即 `exit(0)`），实测**硬杀后 2 秒内自退**；② **daemon 的存活不受我们控制**（实测它既会长期留存、也会自行退出，且 `lark-cli event status` 本身会起一个）→ 故按决策**不去管它**：在就复用、不在就重建，绝不 `kill -9`。**autostart 开关 ✅ 已做**（`autostart_state` / `set_autostart` 命令 + `SystemTab` 开关改为**以 OS 状态为准**；实测 Run 项已写入，且自启起来的 bot 接着原会话继续聊） |

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
| **D1** | ✅ **已定（2026-10-15）：放弃 IP 排他** → 阶段四解锁（`session_guard` 移除或降级为"仅展示在线状态"）。代价：token 成为唯一门槛、桌面"强制踢出"语义消失；`GET /api/session`、`/ping`、`/logout` 与前端 `session-denied` 遮罩随之失去意义。**✅ 已于 2026-10-15 实施（阶段四）**：降级为"仅展示在线状态"，`Denied` 变体删除；文档按 R2 要求**显式记录**"token 成了唯一门槛"，且该结论**只对既定定位成立**（个人助手、同机 `127.0.0.1` + 局域网站内），**将来要对外暴露 webapi 必须先重做鉴权** |

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
| 二 ✅ **完成（2026-10-15）** | `ChatSession.origin`（`ui`/`web`/`lark`；**空串＝未指定**，生效时按 `ui`）+ 按端过滤（list / load / delete / export）+ **写入限同来源端**（跨端写入与删除被拒）+ **50 上限改为按端各自计算**。IPC 恒以 `ui` 身份、webapi 恒以 `web` 身份 —— `origin` **不出现在 IPC 参数里**，避免动参数名（AGENTS.md #4）。旧单会话结构的迁移也补了 `origin` | ✅ 验收：旧文件原样加载且归 `ui`；三端各自只见己方；`None`＝不限来源（供"统一管理"用）。⚠️ **行为变化见下** |
| 三 ✅ **完成（2026-10-15）** | 飞书 `chat_id ↔ session_id` 映射 + `message_id` 去重环（✅ **SQLite 表**，非独立文件）。新增 `commands/sessions/lark.rs`（核心 CRUD，属 §5.5 D4 的"统一管理模块"第二块）+ webapi 接口 `GET/PUT/DELETE /api/lark/binding`、`GET /api/lark/bindings`、`POST /api/lark/seen`（bot 是独立进程，只走 webapi）；`SCHEMA_VERSION` 8 → **9** | ✅ 验收：绑定 upsert 保留 `created_at`、跨私聊互不影响；去重"首次 true / 重投 false"、环容量上限生效。**C3 不由本阶段承担**（映射只是数据），C3 属 bot 自身（P1） |
| 四 | 租约重构（`session_guard` 移除/降级）| ✅ **已实施（2026-10-15）**：**降级**为"仅展示在线状态"—— `GuardOutcome::Denied` 删除、中间件不再 403（只刷新"最近活跃来源"并写日志）、`/api/session`·`/ping`·`/logout`·桌面"强制踢出"保留但**语义已从"权限"变为"查看/清空在线记录"**。**验收：手机浏览器与 bot 可同时使用**（单测已覆盖"异来源必须放行"）；前端**不用改**（它只在 403 时弹遮罩，且不消费 `is_owner`）|

> ⚠️ **阶段二的行为变化（需知悉）**：本功能之前产生的会话一律归 `ui`，因此**在浏览器里打开 Web 端将看不到
> 这些既有会话**（它们仍在 `chat_history.json` 里，桌面端照常可见）。若希望它们对三端都可见，需要额外做一次
> "迁移为共享"的动作 —— **目前未做**，因为定位（§7.1）是三端隔离。飞书 `lark` 端在 P1 接入。
>
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
| **我改了、被应用覆盖** | 多个写入方：手改 vs 应用整文件重写 | **文件监听（热重载）+ 意外覆盖防护** | ✅ 已做（C1：唯一"整份重建"写者 `update_config` 改为**读-改-写**；C3：`config.json` 文件监听热重载，**自写抑制按内容 hash**） |
| **两进程同时写、互相覆盖** | 多进程（CLI / headless / bot sidecar） | **文件锁** | ✅ 已做（C2）：`<config_dir>/config.lock` 上的 `fs4` 排他 advisory 锁（Windows `LockFileEx` / Unix `flock`），OS 在进程退出时自动释放 ⇒ **没有僵尸锁** |

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

**曾待处理（B 档的前置）→ 2026-10-16 已处理**

- **`embed_plans` 的"运行期高频写"** —— 实情比"高频"更糟：`ai::next_embed_plan` 的平台期分支
  每轮都把 `tok_per_s` 乘 0.97（`updated` 也变），于是 `next != prev` **恒真** ⇒
  `embed_batched` 那句"变了就存"让**每次嵌入调用**都要走一遍"跨进程锁 + 读 113 KB + fsync 写回"。
  两处改动：① 计算起点改为**优先读内存计划**（`ai::last_plan_for`，顺带省掉一次整份解析）；
  ② 落盘门槛改为 `ai::plan_should_persist` —— **只看结构性变化**（`batch`/`conc`），
  纯吞吐量/时间戳的衰减留在内存里累计 ⇒ 写盘从"每次调用"降到"每个爬坡台阶"（单测守住）。
- **`web_api_token` 双存储**：**保留**（这是刻意的，不是历史包袱）—— bot 是**另一个进程**，
  只能读 `config.json`；鉴权中间件则每请求从 DB 读（才能"改完立即生效、不必重启"）。
  原先三个入口各写各的，现已**收口到 `webapi::apply_token`**（先写权威的 `config.json`、
  再写 DB、最后作废旧会话），Tauri `update_settings` / Web `PUT /api/settings` /
  启动 `resolve_token` 都只调它 ⇒ 不再有"漏写一侧 → 改了 token 却被 401"。
- **新增写入守卫 `read_config_for_write()`**："读-改-写"的读取**读不懂就返回 `None`、放弃本次写入**
  （文件不存在仍按默认配置处理）。`save_embed_plan` / `set_lark_integration` / `set_web_api_token`
  此前都回退 `AppConfig::default()` —— 等于"读不懂就整份重写"：**一次手改到一半的 `config.json`
  就会被默认值冲掉**（providers / `api_key` / token / 用户未知键全丢）。

> **2026-10-16 决策与进度：走 B 档，分三步落地（C1→C3）**
>
> - **已定**：走 **B 档**（配置文件为中心、允许手改）。飞书策略是**第一块**搬进
>   `config.json` 的集成配置（`integrations.lark`），也是把本节从纸面推到落地的样板。
> - **C1（已合并）**：
>   1. `integrations.lark` 进 `config.json`（`AppConfig.integrations`，`#[serde(default)]`
>      ⇒ **老 `config.json` 原样可加载**，不会因读不懂被判损坏）；
>   2. 启动时把 DB `app_settings` 里 6 个遗留 `lark_*` 键**一次性迁移**过去
>      （幂等标记 `lark_config_migrated`；顺序"先写 config → 再置标记 → 最后删行"，
>      任何一步崩溃重跑都得到同样结果）；
>   3. `lark_*` 从 `update_settings` 的 `ALLOWED_KEYS` **退役** —— bot 只读 `config.json`，
>      写进 DB 的键没人读，那种"改了没反应"正是本文件历史上踩过的静默失败；
>   4. **消除唯一的"整份重建"写者**：`update_config`（AI 设置页）改为**读-改-写**，
>      此后所有写入方都只覆盖自己拥有的字段 —— `integrations` / `embed_plans` /
>      `web_api_token` / 用户手加的未知键**自动**保住，不再依赖"新增字段时记得手工搬一行"。
> - **C2（已合并）**：**跨进程配置锁**。`lock_config()` ＝ 进程内 `CONFIG_LOCK`（**外层**）
>   + `config.lock` 上的 `fs4` 排他锁（**内层**），**整个读-改-写期间**持有 —— 只锁"写"不够：
>   A 读 / B 读 / A 写 / B 写，B 仍会覆盖 A 的改动（原子的写照样能吞掉另一个原子的写）。
>   两个易错点都写进了代码注释：① 句柄**全进程共用**（`fs4` 的锁绑在句柄上：每次新开 fd 会让
>   同进程第二次加锁把自己挡住；而共用句柄时 `try_lock_exclusive` 又会直接成功 ⇒ 进程内互斥
>   必须由外层 Mutex 提供）；② 用 `try_lock_exclusive` + **有界等待**（2s）后**降级**并 WARN，
>   而不是阻塞式无限等 —— 否则一个卡住的 CLI 会冻住界面。跨进程锁是**加固**，不是正确性的唯一来源。
> - **C3（已合并）**：**热重载 + 自写抑制**（`config.rs` / 新模块 `config_watch.rs`）。
>   三个坑都写进了代码注释：① **自写抑制按内容 hash**（`observe_config_content`）——
>   应用自己每次保存都会触发文件事件，按 mtime / 按事件本身判定就会"自己改自己"→ **重载风暴**；
>   ② **监听目录而不是文件** —— 我们的原子写是 `rename` 覆盖，监听文件会在替换后失效；
>   ③ **绝不 panic、绝不阻塞启动** —— watcher 起不来只是"没有热重载"。
>   生效范围（如实记录）：飞书策略 → **重启 bot**（它启动时把策略读进内存，不会自己发现文件变了）；
>   其它配置项本来就每次现读（`load_config()` 无缓存）故天然生效；`web_api_enabled` / 端口这类
>   **进程级**开关仍要重启应用；前端经 `config-changed` 事件刷新（设置页在 C6 已消费）。
> - **C4→C7（已合并，2026-10-16）**：本节的后续落地 —— **C4** 结构化接口
>   （`/api/integrations/lark`，bot 只消费服务端保证的形状）、**C5** 配对模式闭环
>   （闸门 `L1′` + 服务端裁决 `pairing_active`）、**C6** 设置页「集成」面板（含
>   `config-changed` 刷新、Tauri 命令与 Web 端点**共用同一实现**）、**C7** `lark-cli`
>   发现跨平台（不再只认 Windows）。详见 §5.9 与 §5.3 ②。
> - **2026-10-16 补齐（与上一段同一批改动）**：`embed_plans` 落盘门槛（只写结构性变化）+
>   **写入守卫** `read_config_for_write`（读不懂就拒写，不再"整份冲成默认值"）+
>   `web_api_token` **三入口收口**（`webapi::apply_token`）。**两处存储本身保留**（有意为之）。

### 5.7 bot sidecar 的打包（**已解决**：拆 crate → 打开 `externalBin`）

**曾经的死结（2026-10-15 实测）**：`tauri-build` 在**编译期校验** `externalBin`，文件不存在即构建失败
—— 而"生成该文件"的脚本本身必须先编译整个包（含 `build.rs`）→ **鸡生蛋**：

```
error: failed to run custom build command for `link-searcher v1.2.2`
（build.rs = tauri-build 校验 externalBin，文件缺失即失败）
```

根因：**bot 与主程序在同一个 package**，无法"先单独编 bot、再编主程序"。

**解法（当初列的出路 A，已实施）**：把两者拆成**独立 crate**，`src-tauri/Cargo.toml` 升为 workspace 根：

```text
src-tauri/
├── Cargo.toml        # [workspace] members = ["lark-gate", "lark-bot"]；同时仍是主程序 package
├── lark-gate/        # 准入判定（纯策略，只依赖 serde）← 从 commands/sessions/lark_gate.rs 搬家
└── lark-bot/         # bot 本体（只依赖 lark-gate）      ← 从 src/bin/lark-bot.rs 搬家
```

| 收益 | 实测（2026-10-15） |
|---|---|
| 鸡生蛋消失 | `npm run sidecar` 只跑 `cargo build -p lark-bot`，**完全不触碰主程序**（编译列表里没有 tantivy / onnxruntime / Tauri） |
| sidecar 体积 | **30.3 MB → 4.5 MB** |
| `externalBin` | **已打开**（`tauri.conf.json`），`predev` / `prebuild` 自动生成 sidecar |

**为什么 bot 刻意不 import 应用 lib**（三处替代）：

1. **准入判定** → `lark-gate` crate。策略仍只有**一份实现、一份测试**（9 个用例随文件一起搬走）
2. **会话 `messages`** → 不透明的 `serde_json::Value` **原样透传**，不做结构化解析：
   字段一旦与服务端不一致，结构化解析会**静默把历史丢空**（表现为"它忘了前面说过什么"），
   而透传最多被服务端拒绝 —— 那是**能看见**的错
3. **`web_api_token()`** → bot 内的小函数（读 `config.json`，路径规则与 `config.rs::config_dir` 一致），
   另有环境变量 `LS_WEB_API_TOKEN` 优先（便于手动运行）

**两条必须记住的约定**：

- ⚠️ **门禁必须带 `--workspace`**：`src-tauri` 既是 workspace 根**又是** package，不加就**只测主程序包**
  → `lark-gate` 那 9 个准入判定测试会**静默不跑**（而它是"仅私聊 + 白名单"的唯一防线）
- ⚠️ **换机器 / 全新克隆**后先 `npm run sidecar` 再 `cargo test`（`externalBin` 在**编译期**就要文件在）；
  `npm run dev` / `npm run tauri:build` 由 `predev` / `prebuild` 自动兜住，不必记得

`src-tauri/binaries/` 是构建产物，已加入 `.gitignore`。

### 5.8 装机实测（2026-10-15/16，**已通过**）

| 验证点 | 结果 |
|---|---|
| MSI 产出 | `src-tauri/target/release/bundle/msi/Link-Searcher_1.2.2_x64_en-US.msi`（35.3 MB） |
| **安装位置** | `C:\Users\Gray\AppData\Local\Link-Searcher\` —— **perUser 模式，装到 `%LOCALAPPDATA%`，不需要 UAC** ⚠️ **纠正**：先前用 `msiexec /a`（管理安装）解包看到的是 `PFiles\Link-Searcher\…`，**那是管理安装的映射，不代表真实安装位置** |
| 两二进制同目录 | ✅ `link-searcher.exe`（85.41 MB）+ `lark-bot.exe`（3.57 MB） |
| 安装版拉起 bot | ✅ `lark-bot.exe --parent-pid <安装版 PID>` —— 注意 MSI 装的 sidecar 名字是 **`lark-bot.exe`（无 target-triple 后缀）**，正好命中 `bot_exe_path()` 的容错解析（当初若只认带后缀的名字，装完就会找不到 bot） |
| 长连接 | ✅ `lark-cli event consume` 消费者 + daemon |
| **卡片 markdown 降级（安装版实测）** | ✅ 收到的那张：`msg_type=interactive`、页眉「回答」、**裸 `###` = 无**、**加粗 = 有** |
| 有无多余文本答案 | ✅ 无（同刻那条 `text` 是用户自己的提问） |
| 自启 Run 项 | ✅ `HKCU\...\Run` → `Link-Searcher = C:\Users\Gray\AppData\Local\Link-Searcher\link-searcher.exe`（**指向安装路径**） |

> **自启开关的正确操作（坑的升级版）**：Run 项记的是"**当时点开关的那个 exe**"。
> 从 **dev 会话**切到**安装版**时，开关往往**已经是 ON**（只是指向 dev 的 exe）——
> 所以正确做法是 **先关一次、再开一次**（而不是"点一次"），否则会像我这次一样把它点成 OFF。
> 已用 `Get-ItemProperty …Run` 复核：现在指向安装路径 ✅。

### 5.9 结构化接口、命令层与设置页面板（C4/C6，2026-10-16）

**为什么要有"结构化接口"**：飞书策略原先经 `/api/settings` 以**平铺字符串**下发（bool 存成
`"true"`、数组存成 JSON 字符串），每个消费者各自剥壳、各自容错。改成结构化后**形状由服务端
保证**，bot 只消费 —— 也是将来接第二个平台时能复用的接缝。

| 层 | 入口 | 说明 |
|---|---|---|
| Web API | `GET /api/integrations/lark` | 结构化读策略（bot 启动时读它）；**额外带派生字段 `pairing_active`** |
| Web API | `PUT /api/integrations/lark` | 结构化写策略；**写后重启 bot**；只碰 `integrations.lark` 这一块 |
| Web API | `GET /api/integrations/lark/status` | 策略 + **运行态**（与 Tauri `get_lark_status` 同一实现） |
| Web API | `POST /api/integrations/lark/restart` | 手动重启 bot |
| Web API | `POST /api/integrations/lark/pairing/start` \| `…/cancel` | 进入 / 退出配对模式（**截止时刻由服务端算**，不靠 UI 计时） |
| Web API | `POST /api/integrations/lark/pairing/consume` | **配对消费（白名单写入口）** |
| Tauri 命令 | `get_lark_config` / `set_lark_config` | 设置页读写策略 |
| Tauri 命令 | `get_lark_status` | 策略 + **运行态**（`running` / `pairing_active` / 剩余秒数 / 绑定数） |
| Tauri 命令 | `lark_restart` | 手动重启 bot（手改文件由 §5.6 C3 的热重载自动触发，这里是兜底） |
| Tauri 命令 | `lark_pairing_start` / `lark_pairing_cancel` | 进入 / 退出配对模式 |
| Tauri 命令 | `get_lark_bindings` | 已绑定的 p2p 会话 |
| 设置页 | 设置 → **集成**（`LarkTab.tsx`） | 开关 / 白名单 / 外发目录 / 运行态 / 配对按钮 |

> **传输对称**：Web API 那几行与 Tauri 命令**共用** `commands/lark.rs` 的 `pub fn`
> （`status_of` / `save_config` / `pairing_consume` / `pairing_start` / `pairing_cancel`
> / `consume_pairing_into`），命令与端点只做薄包装。因此桌面端与 Web 端**行为一致**，
> 不会出现"桌面能用、浏览器不同"的分叉；前端 `api/client.ts` 的 MAPPINGS 也逐条镜像了这些端点。
>
> ⚠️ **每个写入口都要在写成功后广播 `config-changed`**（`commands/lark.rs::announce_config_changed`）——
> 原因见下面「真机验收 · 缺陷②」：本进程的写会被自写抑制挡掉，`config_watch` **不会**替它补一发。

**三条收在代码里的纪律**：

1. **改策略必须重启 bot**：策略是 bot **启动时读一次**的，不重启＝改了不生效。这条收在
   命令层/端点里，而不是指望每个调用方记得（这是最容易漏的一步）。
2. **配对消费必须先校验配对有效**：`POST …/pairing/consume` 是**白名单写入口**，若不校验就
   等于"凭一个 token 往白名单塞任意 `open_id`"（绕过 L1）。校验口径是 `pairing_active(now)`，
   即 `pairing && pairing_expires_at > now` —— **`0` 也算已过期**（fail-closed：漏设截止时刻
   ＝没开配对，而不是永久敞开）。规则只有一份：`commands::lark::consume_pairing_into`（纯函数，有单测）。
3. **`running` 要问进程，不是问配置**：`lark_bot::is_running()` 用 `try_wait` 顺手回收已退出的
   子进程 —— bot 可能**自己退出**（读到 `enabled = false`、缺 lark-cli、崩溃），只看
   `Option<Child>` 会一直显示"运行中"。

**设置页面板（C6）的两个取舍**：

- **两个名单的文本方向相反**：`allowed_users` 空＝**拒绝全部**；`export_allow_dirs` 空＝**不限制**。
  两个 hint 各自写明，避免"顺手统一"。
- **订阅 `config-changed`**：手改 `config.json`（C3 热重载）、**经服务端的任何写**（本页保存 /
  远程 PUT / bot 配对消费）都会改这份配置，面板据此刷新 —— 否则会一直显示旧值。
  这条链曾断过两处，见下面「真机验收 · 缺陷①②」；前端另有"倒计时归零再问一次服务端"兜底。

**顺带修掉的 W5**：`lark-bot` 是 console 子系统程序，而 GUI 进程没有控制台 → Windows 会为它
**新建控制台窗口**（黑框一闪）。已加 `CREATE_NO_WINDOW`（`lark_bot.rs`）。

> **规范化的单一收口点**：`set_lark_integration` 内部统一做 `sanitized()`（去空白、去重、
> "没在配对就不留截止时刻"）。所有写入口（设置页 / Web API / 一次性迁移 / 手改热重载）都汇到
> 那里，因此调用方**不必各自记得**清理。

**配对链路的完整闭环（C4 端点 + C5 判定）**：

```text
设置页「开始配对」 ──► pairing=true, expires=now+5min ──► 重启 bot
   │
   ▼（用户用飞书私聊机器人一次）
bot 闸门 L2 通过（必须 p2p）──► L1 未命中 ──► L1′ pairing_active ⇒ GateVerdict::Pair(sender)
   │                                              │
   │  **不回答这条消息**                          ▼
   └────────────────────────► POST …/pairing/consume { open_id }
                                   │  校验 pairing_active（失败即拒）
                                   ▼
                              allowed_users += sender; pairing=false ──► 重启 bot ⇒ 白名单生效
```

> 为什么"消费"要重启 bot：策略是 bot **启动时读一次**的 —— 不重启，新配的白名单与
> 已关闭的配对都不会生效。重启会让**正在处理这条消息的** bot 进程退出，因此那次 HTTP
> 响应可能收不到（`Err` 只记 WARN）—— 反正这条消息**本来就不回答**，无影响。

#### 真机验收（2026-10-16，真应用 + 真 bot）

不靠 mock：`npx tauri dev` 起真实应用、真实 `lark-bot` 连真实飞书，用 `curl.exe -sk` 打
`https://127.0.0.1:8443`（自签证书，故用 `-k`）跑 31 条断言 —— **31/31 通过**。

**验过的**（每条都有现场证据，不是"看代码觉得对"）：

| 项 | 结论 |
|---|---|
| C1 迁移（**真实旧数据**） | DB 里原有的 `lark_bot_enabled` / `lark_allowed_users` / `require_p2p` 落进 `config.json` 的 `integrations.lark`；DB 只剩幂等标记 `lark_config_migrated=1` |
| bot 真的起来 | `running:true`；日志 `feishu-websocket: connected` + `ready event_key=im.message.receive_v1` |
| C7 `lark-cli` 解析 | bot 实际加载的就是新解析器给出的路径（本机与旧硬编码同一条，行为不变） |
| C4 结构化接口 | `GET …/lark` 带派生 `pairing_active`；`/status` 带 `running` / `bindings` / `pairing_seconds_left` |
| C5 配对闭环 | start（服务端算截止，`seconds_left=60`）→ 空 `open_id` **400 且配对仍有效** → consume 写白名单并**自动关闭** → 二次 consume **400** → cancel |
| C6 写入规整化 | PUT 去空白 / 去重 / 丢空串（`export_allow_dirs` 里 `D:\index` 重复两次 + 一个空串 → 一条） |
| C3 热重载 | 手改 `config.json` → 日志 `[CONFIG-WATCH] 检测到 config.json 外部改动 → 已重载（飞书策略有变化 → 重启 bot）`，bot 换 PID |
| C3 自写抑制 | 自写后 PID 稳定，**无重启风暴**（否则每次保存都会自触发一轮重载） |
| 鉴权 | 无 token → 401 |

**验收发现的两个缺陷（已修）** —— 都属"桌面看不出来、只在特定路径上错"的那类：

```text
缺陷①  config-changed 没进 BRIDGED_EVENTS
  → 桌面照收（Tauri IPC），浏览器模式**永远收不到** → Web 端设置页永远不刷新。
  证据：端点 `/api/events` 挂着 SSE，手改 config.json，后端日志显示已重载（emit 确实发了），
        但 SSE 只收到 `: ping` 保活，没有任何 `event:` 帧。

缺陷②  本进程的写被自写抑制挡掉，而 emit 只在外部路径
  → 「bot 配对消费」「远程/CLI 改策略」**两种模式都不发事件** → 面板停在旧值
    （配对还显示"配对中"、白名单缺新配的用户）。
  证据：`write_config_file` → `note_known()`（抑制）；`emit` 唯一发射点在 `config_watch::handle_change`。
  这一条与本节早先写的"bot 配对消费成功都会改这份配置、面板据此刷新"**自相矛盾**：
  订阅了也收不到 —— 文档的乐观恰好被真机验收推翻。
```

**修法**：① 把 `config-changed` 加进 `BRIDGED_EVENTS`，并由新测试
`bridged_events_cover_frontend_subscriptions`（`webapi/mod.rs`）守住"**前端订阅的事件必须被桥接**"
（确实桌面专属的要登记进该测试的 `TAURI_ONLY_EVENTS` 并写明理由）；② `set_lark_integration`
改为返回**是否真的变了**，`commands/lark.rs` 在每个写入口成功后 `announce_config_changed`
（`source = "local"`，与外部改动同事件名、同形状）；前端另加"倒计时归零再问一次服务端"兜底
（SSE 断线时也能自愈）。

**修完的实测**（同一条 SSE 流，两帧都到了浏览器侧）：

```text
event: config-changed / data: {"source":"external"}   ← 手改 config.json
event: config-changed / data: {"source":"local"}      ← PUT /api/integrations/lark
```

**没能验的部分（如实记录）**：

- **面板渲染本身**：本机浏览器（含 Agent 的浏览器工具）不接受应用的自签证书
  （证书在 `<data_dir>/tls/cert.pem`，需手动导入信任）；本次只验到"后端事件与接口正确"，
  DOM 渲染由人眼确认。
- **C2 跨进程写锁**：只有**句柄级**单测（`cross_process_lock_is_exclusive_across_handles`，
  忠实于 `fs4` 的 `LockFileEx`/`flock` 语义）；产品里**没有第二个写 `config.json` 的进程**
  （CLI 无集成子命令、bot 只经 HTTP），因此无法真机复现"两个写者"。这不算"验过"。


### 5.10 接入自检与绑定（2026-10-16）—— 回答"新用户怎么绑定？我看不到 appid"

**触发**：用户问「新用户怎么绑定机器人？我没有看到 appid 等内容」。查下来是两个真实断点：
依赖中心的"就绪"**只探可执行文件在不在**（`deps::catalog::lark_cli_path`），"装了但没绑定"照样 ✅；
集成页只有「进程：运行中/未运行」+「详见日志」—— 用户知道去读日志，却**不知道缺哪一步**。

**① 先把"绑定"拆成三层**（只有第三层归本应用）：

| 层 | 内容 | 存在哪 | 本应用有没有界面 |
|---|---|---|---|
| ① 凭证层 | 用哪个飞书应用（App ID / App Secret） | **`lark-cli` 自己的配置**：`%USERPROFILE%\.lark-cli\config.json` → `apps[0]`（Secret 存**系统凭据库**，文件里只留 `{source:"keychain", id:…}` 指针） | 原本**完全没有** |
| ② 授权层 | 事件订阅 / scope / token | 也在 `lark-cli`（`config init` 浏览器流程 + `auth login`） | 原本完全没有 |
| ③ 策略层 | 谁能用、能不能外发、配对 | 本应用 `config.json` 的 `integrations.lark` | ✅ 就是集成标签页 |

**② 关键实测：`lark-cli auth status` 会撒谎，判死活的是 `doctor`**（隔离 `USERPROFILE` 做对照，真配置未被动过）：

```text
# 假 appId（cli_bogus000000000，凭据根本不存在）下：
auth status  → {"appId":"cli_bogus000000000", "identities":{"bot":{"status":"ready"…}}}   EXIT=0   ← 只看本地结构！
doctor       → bot_identity : warn "Bot identity: verify failed: The specified app does not exist."
               identity_ready: fail "no usable bot or user identity is available"          EXIT=1   ← 联网校验
```

→ 所以"能不能用"**一律以 `doctor` 为准**，判据是 `app_resolved` 有 appId **且** `bot_identity = pass`
（**不是** `identity_ready = pass`：那只要求"bot 或 user 有一个可用"，而机器人**必须有 bot 身份**）。
真机 `doctor` 实测 **1.74s**、exit 0、9 项全过（`cli_version` 1.0.96 / `app_resolved` /
`bot_identity` / `user_identity`(warn，本机没登 user) / `identity_ready` / `endpoint_open` / `endpoint_mcp`）。

**③ 第二个关键实测：`config init` 是"先写盘、再校验"** —— 填错会**冲掉原本能用的绑定**：

```text
# 隔离 HOME，故意给不存在的 appId：
stdin: "s3cr3t-bogus" | lark-cli config init --app-id cli_bogus000000000 --app-secret-stdin
→ "OK: Configuration saved to C:\…\.lark-cli\config.json"     ← 已经写进去了
→ {"ok":false,"error":{"type":"config","subtype":"invalid_client","code":20048,…}}   EXIT=3
```

→ 实现里因此有**快照 / 回滚**：绑定前读一次配置，收尾判定**失败即写回**（原本**没有**这个文件就**删掉**，
否则会留下一个"看着已绑定"的假 appId）。收尾判据用**行为**而不是退出码文本：`exit 0` **且**
`doctor` 现在能用。

**④ 实现**（`src-tauri/src/lark_setup.rs`，传输对称：Tauri 命令 + `webapi/routes/lark.rs`）：

| 动作 | 端点 / 命令 | 说明 |
|---|---|---|
| 自检 | `GET …/lark/self-check` · `lark_self_check` | 跑 `doctor` + `event consume card.action.trigger --dry-run`；返回 App ID / 两种身份 / 卡片订阅 / **下一条该做的事**（`install_cli`→`bind_app`→`rebind_app`→`card_callback`，只报第一个阻塞项）+ `doctor` 全部检查项原文 |
| 绑定 | `POST …/lark/bind/start` · `lark_bind_start` | `new`＝`config init --new`（浏览器流程，会**新建一个飞书应用**）；`existing`＝`--app-id` + `--app-secret-stdin` |
| 进度 | `GET …/lark/bind/status` · `lark_bind_status` | 纯内存读；前端在 `running || !finalized` 时每秒拉一次 |
| 取消 / 收起 | `…/bind/cancel` · `…/bind/dismiss` | 取消＝杀掉子进程 + **照样走收尾**（可能已写盘）；取消分支只在"确实新绑上了一个**能用**的应用"时才算成功 |

四条刻意边界：**Secret 只走 stdin**（不进 argv / 进程列表 / 日志 / 本应用配置；前端起步后立即清空输入框）；
**子进程一律带超时**（`doctor`/`dry-run` 都联网，20s 上限）；**收尾的慢操作不持锁**（否则设置页轮询一起卡住）；
**`existing` 模式不允许经 Web API**（自签证书的局域网通道，不该把应用密钥递到网络上）—— 这是**有意的不对称**。

另外：`--new` 在 Agent 工作区（`OPENCLAW_HOME`/`HERMES_HOME`）里会被 lark-cli **拒绝**（其 `--help` 明说），
检测到就给一个 `--force-init` 重试按钮（原文一律进日志，不靠猜）；绑定成功后**重启 bot**，让新凭据立刻生效
（**不自动启用** —— 外发内容必须显式开启）。

**⑤ 新用户接入清单（界面上的顺序就是它）**

```bash
lark-cli config init --new                                  # 1) 绑定：新建应用（浏览器流程）
lark-cli config init --app-id cli_xxx --app-secret-stdin    #    或绑定已有应用（Secret 走 stdin）
lark-cli auth login                                         # 2) 仅当需要 user 身份（bot 收发不需要）
lark-cli event consume card.action.trigger --dry-run        # 3) 卡片按钮：单独订阅（会给出订阅链接）
# 4) 回设置页：启用机器人（12.2）→ 配对把自己加进白名单（12.4）
```

**⑥ 没能验的部分（如实记录）**

- **`config init --new` 的真实浏览器流程没跑过**：它会**在用户租户里真的新建一个应用**，不能拿它做实验。
  因此"验证链接的提取"只有**合成字符串**的单测，真实输出的 URL 形态**未验证**；
  `--force-init` 分支也**从未被触发过**（本机没有 `OPENCLAW_HOME`/`HERMES_HOME`）。
- **`existing` 成功路径没跑过**：只跑了**失败路径**（假 appId → exit 3 → 先写盘后校验，见 ③）。
  用真凭据再 `config init` 会**覆盖**现有可用绑定，属"能重复但会打扰"的操作，本轮没做。
- **应用侧的回滚执行没有真机证据**：失败路径是在**隔离 HOME** 里跑的，`rollback_config` 的两条分支
  （写回 / 删除）只有代码评审 + 纯函数单测（`app_id_from_config` 等）。
- **面板渲染**：由人眼确认（同 5.9 的说明：本机浏览器不信任应用自签证书）。

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

## 七、安全与数据边界（✅ **已定，2026-10-15**）

把本地私有库的内容发到飞书 = **内容离开本机上云**。原本要求"先解决再动手"；现按**个人助手 · 单用户 ·
仅私聊**的定位做出如下决定。

**已定决策**：

| # | 项 | 决定 |
|---|---|---|
| 1 | 外发白名单 | ❌ **放弃** —— 全部可外发（机器人可引用任意文件的正文） |
| 2 | 只回摘要不回原文 | ❌ **放弃**（回答可含原文） |
| 3 | 外发前脱敏 | ❌ **放弃** ⇒ **没有任何内容侧的兜底** |
| 4 | 访问控制 | ✅ **token 认证 + 用户白名单（`open_id`，fail-closed）+ 仅私聊 + 忽略非用户来源** → **完整可实现规格见 §7.1** |
| 5 | 凭据存储 | ✅ 不落我们的 `config.json`，由 `lark-cli` 自持（见下） |
| — | 收紧阀（✅ **保留**） | `lark_export_allow_dirs` **空数组＝不限制**（＝当前行为）；将来要收紧某个目录时**不必改数据模型**。**决定：保留此阀**（零成本留退路） |

**5. 凭据存储**：app_secret **不得进我们的 `config.json`**（它已是明文，且装着 AI `api_key`）。
   实测（2026-10-15）由 `lark-cli` **自持**：Windows 下位于 `%USERPROFILE%\.lark-cli\config.json`
   （`keychain-downgrade` 仅 macOS 有意义）；`lark-cli config show` 对 secret 显示 `****`。
   我们的进程**零接触、不复制**。⚠️ 因此**备份/打包时不要把该文件带上**，它不在 `data_dir` 里，
   容易被"顺手一起备份"带走。

### 7.1 访问控制落地规格（实现按此执行）

> `token` 保护的是"**谁能调用本机 webapi**"，**不**保护"谁能看到机器人发出去的内容"——后者由
> **飞书侧**决定（应用可用范围 + 群成员）。因为闸门 1–3 已全部放弃，**没有任何内容侧兜底**，
> 所以访问控制必须在**代码里**做死。以下是可直接照着实现的完整规格。

**① 判定顺序（严格按此顺序，不要合并）**

```
收到一个事件
  │
  ├─ L0 来源闸门：sender_type != "user"
  │     → 忽略；记 INFO（防"机器人/应用自己的消息"触发自问自答循环）
  │
  ├─ L2 类型闸门：lark_require_p2p == true 且 chat_type != "p2p"
  │     → 拒绝；回复固定提示「本机器人仅支持私聊」；记 INFO
  │
  ├─ L1 名单有效性：lark_allowed_users 解析失败 或 为空数组
  │     → 拒绝全部（fail-closed）；静默；记 ERROR/WARN
  │
  ├─ L1 身份闸门：事件 sender 的 open_id ∉ lark_allowed_users
  │     → 拒绝；静默忽略；记 INFO（含 open_id 与 chat_type，便于事后核查）
  │
  └─ 通过 → 进入正常问答流程
```

**L0 为什么必要**（2026-10-15 实测带出）：群样本里 `mentions[0].id` 是**机器人自己的 open_id**
（`ou_adc10982603e9ca8830853c97c5ba741`）。一旦将来给机器人开"读取群内全部消息"的权限，
它就会**收到自己发出的消息**；若不做来源过滤，就会出现**自问自答的死循环**。
`sender_type` 现在实测为 `"user"`，机器人/应用发出的消息不是这个值。

**为什么必须"先 L2 再 L1"**：群消息会被类型闸门先拦下，日志里就不会把它误记成"某用户不在白名单"，
排查时两类拒绝原因不混淆。

**② 空值语义（最容易写错，务必照做）**

| 键 | 空数组的含义 | 方向 |
|---|---|---|
| `lark_allowed_users` | **拒绝全部**（谁都不许） | **fail-closed** |
| `lark_allowed_chats` | **拒绝全部**（当前不参与判定，见 ④） | **fail-closed** |
| `lark_export_allow_dirs` | **不限制**（全部可外发） | **fail-open** |

> ⚠️ **同是"空数组"，方向相反。** 访问控制若写成 fail-open，等价于**没有访问控制**，而默认配置恰好
> 就是空 —— 这是最危险的组合。**禁止**为"统一风格"把两者改成同向。解析失败（非法 JSON / 非数组）
> 一律按**拒绝全部**处理并记 ERROR：**不能因为"读不懂"就放行**。

**③ 拒绝时的提示策略（故意不对称）**

| 被哪层拒 | 是否回复 | 理由 |
|---|---|---|
| **L2**（非私聊） | ✅ 回固定提示「本机器人仅支持私聊」 | 群里大概率是**你自己**误用，需要即时反馈 |
| **L1**（不在名单） | ❌ **静默** | 不回未知用户：既避免向对方确认"这里有个机器人"，也避免被用来探测 |

**④ 为什么不做 chat 白名单（明确决定）**

`lark_allowed_chats` **保留键名，但当前不参与判定**：

1. **p2p 会话与用户一一对应** → `chat_id` 白名单与 `open_id` 白名单是**同一件事的两种写法**，冗余
2. 两个 fail-closed 名单会立刻产生"**AND 还是 OR**"的歧义 —— 而**歧义就是漏洞**
3. "排除群"已由 L2（`lark_require_p2p`）从**类型**上做掉，不需要再靠 chat 名单

→ 将来若要支持群，再启用该键，并**显式定义**它与 `lark_allowed_users` 的组合语义。

**⑤ 配置键完整规格**

**存储（2026-10-16 起）**：`config.json` 的 `integrations.lark`。原先平铺在 DB `app_settings`
里的 6 个字符串键由**启动时的一次性迁移**搬过来（见 §5.6 C1）；`lark_*` 已从
`update_settings` 白名单退役，DB 键名此后只作历史/迁移来源。

| 字段（`integrations.lark.*`） | 类型 | 默认 | 空值语义 | 用途 |
|---|---|---|---|---|
| `enabled` | bool | **`false`** | — | 总开关。**默认关**，与 `web_api_enabled` 的"默认开"**刻意相反**：把内容发出去必须显式开启 |
| `require_p2p` | bool | **`true`** | — | L2：非私聊一律拒绝 |
| `allowed_users` | 数组（`open_id`） | **`[]`（＝拒绝全部）** | 拒绝全部 | L1：允许触发机器人的用户 |
| `allowed_chats` | 数组（`chat_id`） | `[]` | 拒绝全部 | **当前不参与判定**（见 ④） |
| `export_allow_dirs` | 数组（路径） | `[]` | **不限制** | 内容外发收紧阀 |
| `pairing` | bool | `false` | — | 配对模式（见 ⑥） |
| `pairing_expires_at` | unix 秒 | `0` | — | 配对**截止时刻**；**`0` 或已过去都算失效**（fail-closed：漏设＝没开配对，见 ⑥ 第 4 条） |

> bot **不直接读** `config.json`：它经 `GET /api/integrations/lark` 拿**结构化**结果
> （见 §4.1 的数据流）。形状由服务端保证 —— bot 不再自己剥 `"true"`、也不再解析 JSON 字符串。

**⑥ 冷启动：白名单为空＝谁都不许，那第一次怎么配对？**

默认"拒绝全部"会把自己也锁在外面，解法是**配对模式**（规格）：

1. 设置页首次启用机器人时进入配对模式：`pairing = true`，并写入
   `pairing_expires_at = now + 5 分钟`（截止时刻由**服务端**记，不靠 UI 计时 ——
   用户点完就去干别的了，UI 一关计时就没了）
2. 配对模式下 L1 暂放行，但**仅放行 p2p**，且**不回答**；收到消息即把该消息的 `sender open_id`
   写入 `allowed_users`，随后**自动关闭**配对（消费入口必须校验 `pairing` **仍在有效期内**，
   否则它就成了"凭 token 任意加白名单"的口子）
3. 设置页显示「等待配对：请用飞书私聊机器人一次」
4. **超时**（默认 5 分钟，即 `pairing_expires_at` 到期）自动失效 —— 长期开着等于没有白名单

> 备选（更简单但不推荐）：让用户手动粘贴 `open_id` —— 用户通常不知道自己的 `open_id`，也容易粘错。

**⑦ 测试要求（改动本块必须同时补/更新）**

| # | 用例 | 期望 |
|---|---|---|
| 1 | `chat_type = "group"` 且 `lark_require_p2p = true` | 不回答；回固定提示 |
| 2 | `chat_type = "p2p"`，`open_id` 不在名单 | 不回答；**静默**（无回复） |
| 3 | **空白名单**（`lark_allowed_users = []`） | **拒绝全部**（fail-closed 回归） |
| 4 | 名单**解析失败**（非法 JSON / 非数组） | **拒绝全部** + ERROR 日志 |
| 5 | 白名单命中 + p2p | 正常回答 |
| 6 | `lark_export_allow_dirs = []` | **不限制**（fail-open 回归） |
| 7 | `sender_type = "app"`（机器人自己发的消息） | **忽略**（L0，防自问自答死循环） |
| 8 | 真实群夹具 `oc_5ed092dfb438fc24cd8c2378c15046e2` + `chat_type = "group"` | 拒绝 + 回固定提示 |
| 9 | 配对中 + p2p + 不在名单 | **`Pair`（暂放行但绝不回答）**，把发送者写白名单（= `pairing_lets_stranger_through_but_only_to_consume`） |
| 10 | 配对中 + **群**消息 | 仍然 `DenyNonP2p` —— 配对**不得绕过 L2**（= `pairing_does_not_bypass_type_or_source_gates`） |
| 11 | 配对中 + `sender_type = "bot"` | 仍然 `DenyNonUser` —— 配对**不得绕过 L0**（同上） |
| 12 | 配对中 + **发送者为空**（脏事件） | `DenyNotAllowed`（空 `open_id` 写白名单毫无意义）（= `pairing_ignores_empty_sender`） |

> **第 9–12 条已落地**（2026-10-16，C5）：`lark-gate::gate` 的 `L1′` 分支 + `GateVerdict::Pair(String)`；
> bot 收到 `Pair` 只调 `POST /api/integrations/lark/pairing/consume`，**不回答**。
> "配对是否仍有效"由服务端按 `pairing_expires_at` 裁决后以派生字段 `pairing_active` 下发 ——
> bot 不碰时钟，fail-closed 规则只有一份实现。

> **#3 与 #6 是一对**：它们共同锁死"两个空数组方向相反"这条约定；任一个被改错，测试立刻红。
> **#8 用真实群 id 作夹具**（2026-10-15 实测机器人确实被拉进了该群），比构造的假 id 更有说服力。
>
> **落地进度（2026-10-15）**：判定顺序 ① 与 L0/L2/L1 已实现在 **`lark-gate` 独立 crate**
> （`src-tauri/lark-gate/src/lib.rs`，2026-10-15 从 `commands/sessions/lark_gate.rs` 搬出，见 §5.7），
> 其中 **#1 / #2 / #3 / #4 / #5 / #7**、**#9–#12**（配对，C5）及"@ 前缀剥离"已有测试覆盖。
>
> **仍空着的两条，如实记录（2026-10-16 复核，不假装已解决）**：
>
> - **#6 卡在"没有消费点"，不是"没写测试"**：`export_allow_dirs` 目前**只被存储 + 规整**，
>   `lark-gate::LarkGateConfig` **根本没有这个字段**（它只管"谁能触发机器人"，
>   不管"内容能不能外发"）。因此"空＝不限制"这条约定**至今没有被执行的地方**，
>   也就没有可测的行为 —— 给它写个断言只会是**假测试**（断言规整结果，冒充语义回归）。
>   → 一旦真的出现"内容外发"路径，**必须在同一处**同时落地判定与该用例（fail-open 的方向也要
>   和 `allowed_users` 的 fail-closed 一起写清楚，这一对最容易被"顺手统一"改错）。
>   **2026-10-16 处置：不实现能力，改为在界面与文档里如实描述** —— 设置页那行 hint 此前写着
>   "允许把检索结果写成飞书文档 / 表格的本地目录"，**暗示了一个并不存在的出口**；现四个语言都改成
>   "⚠️ 预留项：当前版本还没有该出口，暂不生效"，`docs/06-settings.md` 同步。值仍会被存储 + 规整
>   （留作将来收紧用的阀门），**仍然没有假测试**。
> - **#8 是**真机**用例，不是单测**：它验的是 bot 收到群消息后**回了固定提示**
>   （`NON_P2P_NOTICE`，`lark-bot/src/main.rs` 的 `DenyNonP2p` 分支）；而 `handle_line` 要真发
>   HTTP + 真回复，属人工验收（与"飞书面板真机点击"同类）。闸门层**等价的 #1 已覆盖判定**，
>   且 L2 **只看 `chat_type`**、不看 `chat_id` —— 真实群 id 夹具对这条判定**不增加信息量**，
>   它的价值在于"这条消息真的来自那个群"（E2E 语境）。
>   → **2026-10-16 已补做（通过）**：见本节 ⑪ 的"群消息真机 E2E"。

**⑧ 实测真实值（2026-10-15，供配对/校验参考）**

| 项 | 值 |
|---|---|
| 机器人 App ID / 名称 | `cli_aa5a2e242e785c09` / 「二飞」 |
| 你的 `open_id` | `ou_bfdf6618eacf1f52eaa1911ca71ace6a` |
| 你的 p2p `chat_id` | `oc_be9fcfffcb9b1b46349fc347c608914b` |
| 回调模式 | `websocket`（长连接） |
| 机器人当前所在群 | **1 个**：`oc_5ed092dfb438fc24cd8c2378c15046e2`「智商在线!」，群主＝**你本人**（`im +chat-list --as bot` 实测 2026-10-16）—— 10-15 记录的"0 个"此后被拉进（见 ③） |

**⑨ 三层防线分工（重申）**

| # | 防线 | 归属 | 可靠性 |
|---|---|---|---|
| ① | L1 + L2（本节规格） | **我们的代码** | ✅ **主防线**，不依赖飞书任何配置 |
| ② | 可用范围 = 仅本人 | 飞书控制台 | 中（挡"别人使用"，**不挡**"群内可见"） |
| ③ | "不允许被加入群聊"开关 | 飞书控制台 | ❌ **实测不存在/无效（2026-10-15）** —— 机器人被**成功**拉进群（`oc_5ed092…`）→ **只剩 ① 可依赖** |

**⑩ 群消息实测（2026-10-15）—— L2 的真实反例**

```
{"type":"im.message.receive_v1","event_id":"740e8b5e…","timestamp":"1792058495213",
 "id":"om_x100b623e0927d0b8ddd38d5edbe1178","message_id":"om_x100b623e0927d0b8ddd38d5edbe1178",
 "create_time":"1792058494768","update_time":"1792058494884",
 "chat_id":"oc_5ed092dfb438fc24cd8c2378c15046e2","chat_type":"group","message_type":"text",
 "sender_id":"ou_bfdf6618eacf1f52eaa1911ca71ace6a","sender_type":"user",
 "content":"@二飞 hello",
 "mentions":[{"key":"@_user_1","id":"ou_adc10982603e9ca8830853c97c5ba741","name":"二飞"}]}
```

| 发现 | 结论 |
|---|---|
| **群消息可达**（被 @ 时） | L2 **必需**；群 `oc_5ed092dfb438fc24cd8c2378c15046e2` 作为**反例夹具**（必须被拒） |
| `mentions[0].id` = `ou_adc10982603e9ca8830853c97c5ba741` | **这是机器人自己的 `open_id`**（被 @ 的对象）→ 催生了 **L0 来源闸门**（见 ①） |
| `content` = `"@二飞 hello"` | ⚠️ 预渲染把 `@_user_1` 换成了 `@二飞` → **送入检索前必须剥掉 @ 前缀**，否则查询词带噪音 |
| 字段集随场景变化 | 群事件多出 `update_time` / `mentions`（p2p 没有）→ 解析必须**宽松**（字段可选），不得假设固定字段集 |
| 唯一稳定的类型判别 | `chat_type`（`"p2p"` / `"group"`）—— L2 就靠它 |

**⑪ 群消息真机 E2E（2026-10-16，**通过**）—— #8 的收尾**

在群「智商在线!」发一条被 @ 的群消息（真实飞书 → 真实 `lark-bot`）：

```text
2026-10-16 14:49:10 WARN [lark_bot] [lark-bot] [bot] 拒绝：非私聊（chat_type=group）
```

| 断言 | 证据 |
|---|---|
| 消息**真的到达** bot（不是夹具） | 上面这行是 `lark-bot` 进程自己打的（`D:\index\app.log`），时间戳为真实到达时刻 |
| 走的是 **L2 分支**（`DenyNonP2p`）而不是"没响应" | 日志文案即 `main.rs` 的 `DenyNonP2p` 分支：`拒绝：非私聊（chat_type=group）` |
| **没有回答**（不该把答案发进群） | 同一时刻**没有**任何 `[bot] 收到卡片动作` / 检索 / 回答类日志 |
| 固定提示已发出 | `reply()` 只在失败时打 `[bot] 回复失败：…`；本次**没有**该行（成功路径不打日志） |

> ⚠️ 据实说明：**"提示是否真的出现在群里"没有自动化证据** —— 那需要在飞书客户端里用眼睛看
> （`reply()` 走 `lark-cli im +messages-reply`，成功时不落任何日志）。日志能证明的是
> "拒绝发生了 + 没有回答 + 回复命令未报错"。

**⑫ 接入自检与绑定（2026-10-16）—— 本节 ⑦ 的"依赖中心看不出授权"落地**

| 项 | 结论 |
|---|---|
| 判据 | **只认 `lark-cli doctor`**（联网校验）。实测：假 appId 下 `auth status` 仍报 `bot ready` + exit 0，而 `doctor` 报 `bot_identity: warn (verify failed…)` + `identity_ready: fail` + exit 1 |
| 判"能用"的确切条件 | `app_resolved` 有 appId **且** `bot_identity = pass`（**不**用 `identity_ready`：user-only 也算 pass，但机器人必须有 bot 身份） |
| 绑定 | 代跑 `config init`：`--new`（新建应用，浏览器流程）/ `--app-id` + `--app-secret-stdin`（已有应用） |
| ⚠️ 冲库风险 | `config init` **先写盘再校验**（实测 exit 3 时已写盘）→ 绑定前**快照**、失败**回滚**（无原文件则删除） |
| ⚠️ Secret 边界 | 只走 **stdin**（不进 argv / 进程列表 / 日志 / 本应用配置）；`existing` 模式**不允许经 Web API**（自签证书的局域网通道） |
| 未验 | `--new` 真实浏览器流程（会在用户租户里真建应用，不能拿来做实验）、`existing` 成功路径、`--force-init` 分支 —— 详见 §5.10 ⑥ |

**（附）拒绝推进的边界**：若将来要进群 / 多人使用，必须回到闸门 1–3（外发白名单、只回摘要、脱敏），
并启用 `lark_allowed_chats` 且**显式定义**其与 `lark_allowed_users` 的组合语义 ——
那时的结论**不适用**于本节的"全部可外发"。

---

## 八、另外几个必须处理的技术点

| 问题 | 说明 | 做法 |
|---|---|---|
| **慢回答的体验** | RAG 要几秒到几十秒；飞书有消息长度、卡片更新次数、QPS 限制 | 先回"处理中"卡片 → 用 patch 逐步更新。**能用的 SSE 只有两条**：`POST /api/ai/conversation/ask/stream`（按 session 推流）与全局 `GET /api/events`（含 `ai-chunk`/`ai-progress`/`ai-done`）。`/api/ai/events` 是普通 GET，只能轮询已落库事件 |
| **幂等** | 事件可能重投（官方文档明确提到订阅泄漏会导致重复投递） | 按 `message_id` 去重，否则重复回答。**2026-10-15 实测证实**：`id` 与 `message_id` 相同（前者是 legacy alias），而 `event_id` 不同、且官方明言**不得**用作去重键 |
| **离线感知** | GUI 退出/重启时 webapi 不可用，机器人会"静默失联" | 健康检查 + 向指定群告警 |
| **卡片更新次数上限** | 卡住 P1 的真实限制 | `card.action.trigger` 的 token **有效期 30 分钟、最多更新 2 次**（`lark-cli event schema card.action.trigger` 原文；**2026-10-15 实测回调里确实带 `token`**）；飞书对单条消息的卡片更新次数另有限制。**不能每个 SSE chunk 都 patch 一次** → 必须**节流合并**（按时间或 token 数批量更新），否则必然超限。另：`card_content` 会被飞书**重写**（非原文回显，见 §2 实测发现 ②） |
| **会话续租** | 机器人是长时间常驻进程 | 见 §5.1「隐性硬前提」：同机 `127.0.0.1` + 周期 `POST /api/session/ping`，否则 600s 后被 403 |

### 8.1 卡片三件事的实测澄清（2026-10-15，探针实测）

> 起因：`lark-cli im` 下**没有 card 子命令**（`+messages-send` / `+messages-reply` 只列 text/markdown/post/media），
> 于是先探"卡片到底能不能发/更新"，结果把上表那条"**必然超限**"的假设也一并修正了。

| # | 未知数 | **实测结论** |
|---|---|---|
| ① | raw `api` 逃生口能否用 **bot 身份**发卡 | ✅ **可以**：`lark-cli api POST /open-apis/im/v1/messages --as bot`（`im` 无快捷命令，**必须走 raw**） |
| ② | 卡片 JSON **schema 版本** | ✅ **1.0（`elements` 顶层）与 2.0（`schema:"2.0"` + `body.elements`）都被接受** |
| ③ | **patch 更新次数上限** | ✅ **80 次连续全成功**（250–300ms 间隔，**未见次数上限**）→ 见下"修正" |
| ④ | `--data` 传大 JSON | ✅ 支持 **`@file`**（也有 `-` 走 stdin）→ 避开 argv 转义与长度两个坑 |
| ⑤ | 能否**回复**卡片（而非裸发） | ✅ `POST /open-apis/im/v1/messages/{message_id}/reply` + `msg_type=interactive` 成功 |

**对 §8 上表的修正**：

- 原文"飞书对单条消息的卡片更新次数另有限制……**否则必然超限**"——**实测不成立**（至少到 80 次无失败）。
  §8 记的"**最多更新 2 次**"是 **`card.action.trigger` 回调 token** 那条路（用于"用户点了按钮后更新那张卡"），
  **不是** app 身份 `PATCH /open-apis/im/v1/messages/{message_id}` 这条路。两件事**必须分开**。
- app 身份 patch 的真实约束是**频率（QPS）而非次数** → 节流仍然要做（避免贴脸打 + 省流量），
  但**不必**按"只有 2 次"来设计；实测 250ms 间隔完全安全。

**另一个必须记的坑（我第一版探针就栽在这）**：`lark-cli` 的返回封装是
```json
{ "ok": true, "identity": "bot", "data": { … }, "_notice": { … } }
```

**不是**飞书的 `{ "code": 0, "msg": "success", "data": … }`。按 `code === 0` 判成功会把**成功读成失败**
（实测：patch 明明成功，却因 `code` 为 `undefined` 被记成失败）。判据要用 **`ok === true`**。
另外每次调用都会带 `_notice.update`（1.0.96 → 1.0.98 的升级提示）—— 按决策**不升级**，忽略即可。

### 8.2 卡片 + 节流的**实现**与实测（2026-10-15）

已落地在 `src-tauri/lark-bot/`（`main.rs` + `card.rs`，22 个单测）：

```text
收到提问 → 回复一张「正在检索…」卡片（reply + msg_type=interactive）
        → 消费 POST /api/ai/conversation/ask/stream（SSE：ai-progress / ai-chunk / ai-done）
        → **节流** patch 卡片 → ai-done 收尾成「回答」卡片（超长则**分多张**）
```

| 决策 | 取值 | 依据 |
|---|---|---|
| 节流 | 最小间隔 **1.5s**、中间更新上限 **12** 次、**收尾必发**；不到间隔的中间态**合并丢弃** | §8.1：约束是**频率**而非次数（250ms 也安全，1.5s 留足余量） |
| 卡片 schema | **1.0**（`elements` 在顶层） | §8.1：1.0/2.0 都被接受，取更保守的 |
| 正文预算 | **按字节** 20KB（≈6500 汉字），按**字符边界**回退 | 飞书硬限是卡片 JSON **≤30KB 字节**（中文 1 字 = 3 字节） |
| 超长回答 | **分多张卡片**：第 1 张（流式那张）装满 → 续发 `回答(2/N)`…，上限 **6 张**，再超才合并+给桌面端指引 | "过长也要能完成" |
| 流式 vs 非流式 | 改走**流式**（顺带修掉"「他」指代不明确"） | 非流式路由**忽略** `sessionId`，流式路由**接收**它 → 证据按轮绑定 |
| 兜底 | 卡**未发出**前的失败 → **完全退回纯文本**；卡**已发出**后的失败 → 一张「出错了」卡片 | 避免用户同时看到卡片 + 文本两份答案 |
| 逃生阀 | `LS_LARK_CARDS=off` 强制纯文本 | 排查卡片问题用 |

**实测（真实飞书，2026-10-15）**：卡片创建成功、**`updated: true`（节流更新确实发生）**、`reply_to` 是回复、
页眉「回答」、页脚「共用 19 条证据 · 67s」、**没有**多余的文本答案 ✅。
**探针**：飞书接受 **19961 字节**的卡片（`ok: true`）→ 20KB 预算安全 ✅。

**踩过的一个我自己的错（写下来防复发）**：卡片正文最初按 **2500 字符**截断 —— 比纯文本的 3000 字**还保守**，
而飞书卡片本可承载约 6500 汉字。根因：**按字符给"字节限制"设上限**（中文 1 字 3 字节，等于把预算少算了 3 倍）。
→ 结论：**受字节约束的限制就按字节设**。

**两个 dev 工作流的坑（已入 AGENTS.md ⑤″ / ⑤‴）**：
① `tauri dev` **不重建** sidecar（bot 是独立 crate、不是应用包的依赖）→ 改 bot 源码后**必须重启 dev**；
② 构建与运行**互锁**：运行中的 bot 锁着 `target/debug/lark-bot.exe`，而 `tauri-build` 要复制它 →
`cargo test` 报 `PermissionDenied (os error 5)` → **门禁前先停应用**。

### 8.3 飞书卡片 markdown 的**真实子集**（2026-10-15 实测矩阵，用户逐项核对）

卡片正文一直是 `{"tag":"markdown"}`，所以问题不是"能不能渲染 markdown"，而是**飞书支持哪几种子集**。
发了一张编号 ①–⑰ 的探针卡片请用户逐项核对，结论如下：

| 支持 ✅ | **不支持 ❌** |
|---|---|
| `**粗体**`、`*斜体*`、`~~删除线~~`、`- ` 列表、`1. ` 列表、**嵌套列表（缩进）**、代码块、链接、`---` 分割线、空行保留 | **`#` 标题**（① ② ③）、**行内代码**（⑦）、**`>` 引用**（⑪）、**表格**（⑫） |

> **修正（用户复核）**：⑩ **嵌套列表是支持的** —— 因此缩进必须**原样保留、不许拍平**；
> 这条已落实在实现里（`*`→`-` 只换标记符，前缀空格不动）。

**为什么这条必须要做**：模型的回答里**标题极多**（`### 一、公司设立…`），而飞书不渲染 `#`
→ 用户在真实卡片里看到的就是**满屏裸 `###`**（首张真实卡片的 JSON 里也确实是 `###`）。

**降级规则**（`card::normalize_for_card`，纯函数 + 6 个单测；**只改"怎么摆"，不改内容**）：

| 原写法 | 降级为 | 理由 |
|---|---|---|
| `### X` | `**X**`（独立加粗行，前面补空行） | 飞书不渲染 `#`；加粗 + 空行仍能读出层级 |
| `* 项目` | `- 项目`（缩进保留） | `*` 会被当成斜体起始；`-` 实测稳定。**要求 `*` 后是空白**，否则 `*斜体*` 会被误伤 |
| `` `x` `` | `x` | 行内代码不渲染 → 留着就是裸反引号 |
| `> 引用` | 去掉 `>`，保留文字 | 引用块不渲染 |
| `\| a \| b \|` 表格 | **用代码块包起来** | 不渲染表格；代码块能保住对齐 |

**安全边界**：**代码块内部一律原样保留**（`#`、反引号、`|` 在那里都是内容），并有单测盯着；
归一化**幂等**（跑两次结果相同）。层级的表达降级为"空行 + 加粗 + 中文编号（一、二、三）"。

### 8.4 卡片交互（"D"）的前置调研与设计（2026-10-16）

**本次实测/复核的事实**：

- `lark-cli event consume <EventKey>` **只接受一个 key**（没有多 key 语法）→ 卡片回调必须**另起一个 consumer**。
- `event consume card.action.trigger --as bot --dry-run` → **`status: ready`**，三个前置全部 ok
  （`credentials_available` / `console_event_published` / `scopes_granted`），其 `would_write` 是
  `start_or_reuse_local_bus` + `register_consumer` + `open_event_stream` ——
  即它**注册到本地 bus**，而**不是**自己再连一条飞书上游。
- 因此 §5.1 那条"同一 app **只能有一个** event bus 连接"应当指的是**上游**（daemon → 飞书）那一条；
  **本地多消费者大概率是允许的** —— 但**这条还没有被直接验证**。

**决定性实验：已做，通过（2026-10-16）** ✅

在 bot 的 consumer（`im.message.receive_v1`）**正跑着**时另起：

```text
lark-cli event consume card.action.trigger --as bot --max-events 1 --timeout 25s
→ [event] ready event_key=card.action.trigger
→ [event] exited — received 0 event(s) in 25s (reason: timeout)，exit 0
```

**结论：本地多消费者是允许的** —— §5.1 那条"同一 app 只能有一条 event bus 连接"确实指**上游**
（daemon → 飞书）那一条；另起消费者之后，**bot 自己的 consumer 仍在正常运行**（实测未被干扰、
也不影响正在服务的消息流）。
→ **D 按"双消费者"架构实现**（不需要另找实现路径）。

**若实验通过，D 的实现要点**：

1. bot 起**两个** consumer（消息 + 卡片回调），各自一条 NDJSON 流（失败互不影响）；
2. 回调路由用 **`action_value` / `custom_action_id`** —— §2 实测 `card_content` 会被飞书**重写**，
   **不能**靠"比对发送原文"判断卡片状态；
3. 更新卡片**优先用 app 身份 `PATCH`**（§8.1 实测**无次数限制**），
   **不要**走回调 token 那条路（"最多 2 次"，那是另一条路）；
4. 按钮语义（§6 路线）：`打开详情`（链到 WebUI）/ `换一批证据`（重新检索）——
   注意与"回答已收尾"的状态机配合（回调可能**晚到**，要能忽略过期回调）。

> **另**："分页"已由**自动续发卡片**解决（§8.2）→ D 的按钮应做**交互类**（详情 / 重试），
> 而不是翻页按钮（那是重复建设）。

**⚠️ 实现坑（2026-10-16 修）：回调消费者的子进程被"自己"提前 drop ⇒ 按钮**从未**可用**

按"双消费者"落地后，真机日志每次启动都成对出现（间隔 < 1 秒）：

```text
[bot] 卡片回调长连接就绪
[bot] 卡片回调 stdout 结束（按钮将不可用）
[lark-cli] [event] … stdin closed → shutting down          ← 子进程自己说"stdin 被关了"
[lark-cli] [event] exited — received 0 event(s) in 0s (reason: signal)
```

**根因**：`spawn_card_consumer()` 里 `let mut child = spawn_consumer(…)`，只把 `stdout`
移进了读取线程，**函数一返回 `child` 就被 drop** ⇒ 它持有的 **stdin 管道被关闭**；而 lark-cli 的
**优雅停机方式就是"关 stdin"**（Windows 没有可用的 SIGTERM，见 `spawn_consumer` 上方的注释）
—— 于是子进程收到 EOF 立刻自杀。消息主链路没事，只是因为 `main()` 把它的 `child` 一直攥着。

**修法**：把 `child` 也 move 进读取线程（喂给 `BufReader` 的仍是 `stdout`），读完循环后
`child.wait()` 回收。**教训**："起子进程"和"**保活**子进程"是两件事 —— 只搬走一个管道，
等于在函数返回时亲手按下了停机信号。这条也解释了"为什么 §8.4 的双消费者实验**是通的**
（手工在 shell 里跑，stdin 没被关），而**产品里按钮一直是死的**"。

---

## 九、分阶段实施建议

| 阶段 | 内容 | 前置 |
|---|---|---|
| **P0** ✅ **完成（2026-10-15）** | `config init --new` 建应用并授权（App ID `cli_aa5a2e242e785c09`）；已拿到真实样本并逐字段确认；**收 + 发闭环已验证** —— 全部记录见 §2「P0 实测记录」 | 前置由浏览器授权流程**一并满足**，无需手工补控制台事件/scope |
| **P0.5** ✅ **完成（2026-10-15）** | ① ✅ 身份实测为 **`bot`**（`defaultAs: auto` 解析到 bot）<br>② ~~同机单会话租约实测~~ **已作废**（D1 已定＝放弃 IP 排他，租约将在阶段四拆除）<br>③ ✅ **`card.action.trigger` 的 `callback` 类型确认可走长连接**（样本与 4 条实测发现见 §2） | P0 ✅ |
| **P1** 🟡 **进行中（2026-10-15）** | 已落地 **bot sidecar 纯文本版**并在真实飞书上**端到端验证通过**（去重 → 绑定 → 取历史 → RAG 问答 → 落库 → 回复；多轮连续亦成立）。**待做**：① 卡片 + 节流更新（§8）② **autostart 开关**（带 data_dir）③ **C3 生命周期** ④（可选）内容闸门 —— 注意 §7 已放弃"只回摘要/脱敏"，故现在会**原样外发**含身份证号等的正文 | 数据边界已定（§7） |
| **P1-a（可选决策）** | **内容闸门**：命中身份证/手机号等正则则不外发，只回"涉敏，请在桌面端查看" | 需你决定是否加（§7 已放弃脱敏，故目前无内容侧兜底） |
| **P2** | 文件 / 语音 / 图片入站（下载 → 监控目录 → 自动入库 → 回执） | P1 稳定 |
| **P3** | 定时摘要 + 出口（写飞书文档/表格）+ 卡片回调 | P0.5 ③ 已验证 `card.action.trigger` 可用 |
| **P4**（可选） | headless `serve` 模式（方案 B），用于无 GUI 的机器 | 明确目标机器平台；**先做 Windows 无窗口 spike** |

---

## 十、待确认事项

> **2026-10-15 收口**：绝大多数已定。未定项在下方单列。

**已定**

- [x] **定位** ✅：**个人助手（方案 A）· 多端同账号**（＝ §5.4 场景 B）
- [x] **目标机器** ✅：**桌面常驻**，跨平台均可（**不是** Windows 服务、**不是** Linux NAS）
- [x] **机器人部署位置** ✅：与 webapi **同机 `127.0.0.1`**
- [x] **数据边界** ✅：**全部可外发**（放弃 §7 的闸门 1–3 —— 即无内容侧兜底）；**保留** `lark_export_allow_dirs` 空收紧阀
- [x] **访问控制** ✅：**token 认证 + 仅私聊、不得进群** → **落地要求 3 项见 §7，未完成前"安全"是假象**
- [x] **身份** ✅：`bot`（`defaultAs: auto` 解析到 bot）
- [x] **卡片回调** ✅：`card.action.trigger` 可走长连接（P0.5 ③，见 §2）
- [x] **GUI 与 headless 互斥** ✅：**不做方案 B** —— 定位是"桌面常驻"，headless `serve` 不在范围内，该问题自然消解
- [x] **bot 形态** ✅：**Rust sidecar**（同一 build、可复用 `process.rs`、`externalBin` 一站式打包；并绕开"用 shell 调 `lark-cli` 传 JSON"的引号坑）
- [x] **会话映射存储** ✅：**SQLite 表**（`data.db`），不用独立文件 —— 事务性 + 跨进程安全，不必再造一套锁与临时文件

**仍未定（均不阻塞 P1）**

- [ ] **跨平台打包范围**：范围是"跨平台"，但**打包当前只有 MSI**（无 dmg / deb / appimage，无 CI）。要在 macOS/Linux 装机需补打包流程。**Windows MSI 现已内含 bot**（`externalBin`，2026-10-15 实测：`Link-Searcher_1.2.2_x64_en-US.msi` 解包后 `lark-bot.exe` 与主程序同在 `Link-Searcher\` ✅）
- [x] **`lark-cli` 的取得方式** ✅：**不随包分发**（省 49 MB）；**纳入「依赖中心」管理** —— 复用 `deps/catalog.rs`
  那一套（探测是否已安装 / 状态展示 / 按需安装 / 取消），并在杀软与安全中心**加信任**

> ✅ **已确认（2026-10-15）**：`lark-cli` 的取得方式就是**依赖中心**（`deps/catalog.rs` 那套），
> 不随包分发、在杀软/安全中心加信任。此前的"待澄清"已作废。
