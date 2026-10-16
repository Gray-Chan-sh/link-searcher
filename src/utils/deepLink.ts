/**
 * Web 端深链参数解析（飞书回答卡片「打开详情」用）。
 *
 * 链接形状：`https://<lan-ip>:<port>/#/chat?origin=lark&session=<会话 id>`
 * —— 应用用的是 `HashRouter`，query 在 **hash** 里，所以调用方传「hash 中的 query 串」
 * （`useSearchParams().toString()` 对两种路由都能取到它）。
 *
 * 为什么必须带 `origin`：会话按来源端隔离（`docs/LARK_BOT.md` §5.5 阶段二）。
 * 不带它去读飞书会话只会得到 `session not found`。
 */

export type DeepLinkOrigin = 'ui' | 'web' | 'lark'

export interface DeepLink {
  /** 页面来源端：决定「列会话 / 建会话」往哪个端查（会话自身带 origin，读写跟会话走）。 */
  origin: DeepLinkOrigin
  /** 要直达的会话 id；`null` 表示只按 origin 正常列会话。 */
  session: string | null
}

const ORIGINS: readonly DeepLinkOrigin[] = ['ui', 'web', 'lark']

/**
 * 解析 query 串。**不是深链**（两个参数都没有）时返回 `null`，
 * 让调用方保持原有行为 —— 桌面端、以及直接打开 WebUI 的路径都不受影响。
 *
 * 非法输入一律降级而不报错：`origin` 只认白名单（否则当没有），
 * `session` 只做去空白（"存不存在"由服务端判定并回错）。
 */
export function parseDeepLink(query: string): DeepLink | null {
  if (!query) return null
  let params: URLSearchParams
  try {
    params = new URLSearchParams(query)
  } catch {
    return null
  }
  const rawOrigin = (params.get('origin') ?? '').trim().toLowerCase()
  const origin = ORIGINS.includes(rawOrigin as DeepLinkOrigin) ? (rawOrigin as DeepLinkOrigin) : null
  const session = (params.get('session') ?? '').trim() || null
  if (!origin && !session) return null
  return { origin: origin ?? 'web', session }
}
