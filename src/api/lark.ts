import * as client from './client'

/**
 * `config.json` 的 `integrations.lark`（与 Rust `LarkIntegration` 同形）。
 *
 * 字段名是 **snake_case**：服务端与 bot 都按原名收发（不像 DB `app_settings` 那样
 * 把 bool 塞成 `"true"`、数组塞成 JSON 字符串 —— 结构化接口就是为了不再各自剥壳）。
 */
export interface LarkIntegration {
  enabled: boolean
  require_p2p: boolean
  /** 允许触发机器人的 `open_id`；**空 ⇒ 拒绝全部**（fail-closed）。 */
  allowed_users: string[]
  /** `chat_id` 白名单；**当前不参与判定**（群聊尚不支持）。 */
  allowed_chats: string[]
  /** 内容外发收紧阀；**空 ⇒ 不限制**（方向与上面相反）。 */
  export_allow_dirs: string[]
  /** 配对模式标志（是否**此刻**有效由服务端按 `pairing_expires_at` 裁决）。 */
  pairing: boolean
  /** 配对截止时刻（unix 秒）；**`0` 或已过去都算失效**。 */
  pairing_expires_at: number
}

/** 策略 + **运行态**（Rust `commands::lark::LarkStatus`）。 */
export interface LarkStatus {
  config: LarkIntegration
  /** `enabled` **且** bot 进程真的在跑。 */
  running: boolean
  /** 服务端裁决后的"配对此刻是否有效"。 */
  pairing_active: boolean
  pairing_seconds_left: number
  /** 已绑定的 p2p 会话数。 */
  bindings: number
}

/** p2p `chat_id` ↔ 本库聊天会话。 */
export interface LarkBinding {
  chat_id: string
  session_id: string
  user_open_id: string
  created_at: number
  updated_at: number
}

export async function getLarkConfig(): Promise<LarkIntegration> {
  return client.invoke<LarkIntegration>('get_lark_config')
}

/** 写策略（服务端会**重启 bot** —— 否则改了不生效）。 */
export async function setLarkConfig(lark: LarkIntegration): Promise<void> {
  return client.invoke('set_lark_config', { lark })
}

export async function getLarkStatus(): Promise<LarkStatus> {
  return client.invoke<LarkStatus>('get_lark_status')
}

/** 手动重启 bot（手改 `config.json` 由热重载自动处理，这里是兜底入口）。 */
export async function restartLarkBot(): Promise<void> {
  return client.invoke('lark_restart')
}

/** 进入配对模式；`minutes` 缺省由服务端取 5，并夹到 `1..=60`。 */
export async function startLarkPairing(minutes?: number): Promise<void> {
  return client.invoke('lark_pairing_start', { minutes: minutes ?? null })
}

export async function cancelLarkPairing(): Promise<void> {
  return client.invoke('lark_pairing_cancel')
}

export async function getLarkBindings(): Promise<LarkBinding[]> {
  return client.invoke<LarkBinding[]>('get_lark_bindings')
}

// ───────────────── 接入自检 / 绑定（`src-tauri/src/lark_setup.rs`） ─────────────────

/** `lark-cli doctor` 的一条检查项（**原样**下发，不做"看起来不错"的加工）。 */
export interface LarkCheck {
  name: string
  /** `pass` / `warn` / `fail`（lark-cli 的原话）。 */
  status: string
  message: string
}

/**
 * 接入自检结果。
 *
 * ⚠️ **`healthy` 是联网校验出来的**：`lark-cli auth status` 对假 App ID 也报 ready
 * （只做本地结构检查），所以"到底能不能用"一律看这里的 `doctor` 结果。
 */
export interface LarkSelfCheck {
  /** `lark-cli` 可执行文件路径；`null` = 没装。 */
  cli_path: string | null
  cli_version: string | null
  /** 已绑定应用的 App ID（用户"看不到"的就是它）。 */
  app_id: string | null
  brand: string | null
  healthy: boolean
  bot_status: string
  bot_message: string
  user_status: string
  user_message: string
  /** `ready` / `blocked` / `unknown`。 */
  card_callback: string
  card_callback_detail: string
  checks: LarkCheck[]
  /** `install_cli` / `bind_app` / `rebind_app` / `card_callback`；空 = 前置齐了。 */
  missing: string[]
  error: string | null
}

/** 绑定请求（`new` / `existing` 二选一）。 */
export interface LarkBindRequest {
  mode: 'new' | 'existing'
  app_id?: string | null
  /** ⚠️ 只经 stdin 交给 lark-cli，本应用不落盘、不打日志。 */
  app_secret?: string | null
  brand?: string | null
  force_init?: boolean
}

/** 绑定会话快照（会话期间前端每秒拉一次）。 */
export interface LarkBindStatus {
  /** `new` / `existing`；**空串 = 没有会话**。 */
  mode: string
  running: boolean
  /** 收尾（判定成功 / 回滚）也做完了。`running || !finalized` 期间才需要轮询。 */
  finalized: boolean
  /** 从输出里捞到的验证链接（`--new` 用）。 */
  urls: string[]
  /** 原始输出（stdout + stderr）。 */
  log: string[]
  exit_code: number | null
  /** `exit 0` **且** `doctor` 确认现在这个绑定真的能用。 */
  success: boolean
  app_id_after: string | null
  /** 检测到 lark-cli 的"Agent 工作区拒绝新建"分支 → 给 `--force-init` 重试。 */
  needs_force_init: boolean
  error: string | null
}

/** 接入自检（**只读**；每次会起子进程跑 `doctor`，约 2s、含联网）。 */
export async function larkSelfCheck(): Promise<LarkSelfCheck> {
  return client.invoke<LarkSelfCheck>('lark_self_check')
}

/** 开始绑定；**不阻塞**，进度靠 {@link larkBindStatus} 拉。 */
export async function larkBindStart(req: LarkBindRequest): Promise<void> {
  return client.invoke('lark_bind_start', { req })
}

export async function larkBindStatus(): Promise<LarkBindStatus> {
  return client.invoke<LarkBindStatus>('lark_bind_status')
}

/** 取消绑定；返回是否真的取消了一个在跑的会话。 */
export async function larkBindCancel(): Promise<boolean> {
  return client.invoke<boolean>('lark_bind_cancel')
}

/** 收起已结束的绑定会话。 */
export async function larkBindDismiss(): Promise<void> {
  return client.invoke('lark_bind_dismiss')
}
