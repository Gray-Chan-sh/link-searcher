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
