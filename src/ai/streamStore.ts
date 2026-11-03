import * as client from '../api/client'
import { normalizePath } from '../utils/normalizePath'
import { loadChatSession, saveChatSession, type AiDonePayload } from '../api/files'
import { translate } from '../i18n'
import { buildSessionFromDone } from './applyDone'

type StreamState = { text: string; reasoning: string }

/** 单条陈述的核验结论（后端 `answer_verify` 事件）。 */
export type VerifyVerdict = 'supported' | 'contradicted' | 'absent' | 'derived' | 'unchecked'

export type VerifyItem = {
  id: number
  sentence: string
  cites: number[]
  verdict: VerifyVerdict
  /** 含可核对数值（日期/金额/文号）。`false` ＝ 叙述型，结论仅供参考。 */
  numeric: boolean
  correct?: string
  note?: string
}

export type AnswerVerifyPayload = {
  session_id: string
  trace_id?: string
  checked: number
  supported: number
  contradicted: number
  absent: number
  derived: number
  unchecked: number
  took_ms: number
  /** 只含非 `supported` 的明细（`supported` 用计数表示）。 */
  items: VerifyItem[]
}

const active = new Set<string>()
const streams = new Map<string, StreamState>()
const dones = new Map<string, AiDonePayload>()
// 依据核验是作答之后的旁路结果，可能比 `ai-done` 晚数秒，单独保存供面板渲染。
const verifies = new Map<string, AnswerVerifyPayload>()
const listeners = new Map<string, Set<() => void>>()
// 正在由 store 代为落库的会话：期间挂载的 ChatPanel 不得清 pending（会同文件竞争写）
const persisting = new Set<string>()
const watchdogs = new Map<string, ReturnType<typeof setTimeout>>()
let started: Promise<void> | null = null

// Long local prefills can legitimately run for several minutes. This is an
// inactivity watchdog, reset by every chunk; it only exists so a lost
// `ai-done` can never leave the panel stuck forever.
const STREAM_WATCHDOG_MS = 10 * 60 * 1000

function clearWatchdog(sessionId: string): void {
  const timer = watchdogs.get(sessionId)
  if (timer) {
    clearTimeout(timer)
    watchdogs.delete(sessionId)
  }
}

function armWatchdog(sessionId: string): void {
  clearWatchdog(sessionId)
  const timer = setTimeout(() => {
    watchdogs.delete(sessionId)
    if (!active.has(sessionId)) return
    console.warn('[streamStore] stream watchdog fired; clearing stale state', sessionId)
    active.delete(sessionId)
    streams.delete(sessionId)
    notify(sessionId)
  }, STREAM_WATCHDOG_MS)
  watchdogs.set(sessionId, timer)
}

export function isPersisting(sessionId: string): boolean {
  return persisting.has(sessionId)
}

/** 最近一次依据核验结果（未核验过则 `undefined`）。 */
export function getAnswerVerify(sessionId: string): AnswerVerifyPayload | undefined {
  return verifies.get(sessionId)
}

/**
 * 用户已切走、无 ChatPanel 订阅时，由 store 直接把 `ai-done` 结果写回会话。
 * 这是「切页后回答丢失」的兜底：即使之后不再回到聊天页，回答也已持久化。
 * 返回是否写入成功。
 */
async function persistDone(sessionId: string, e: AiDonePayload): Promise<boolean> {
  persisting.add(sessionId)
  try {
    const cur = await loadChatSession(sessionId)
    if (!cur) return false
    await saveChatSession(buildSessionFromDone(cur, e, translate))
    return true
  } catch (err) {
    console.error('[streamStore] persist done failed:', err)
    return false
  } finally {
    persisting.delete(sessionId)
  }
}

async function handleDone(raw: AiDonePayload): Promise<void> {
  const sessionId = raw.session_id
  clearWatchdog(sessionId)
  active.delete(sessionId)
  const e: AiDonePayload = {
    ...raw,
    source_files: raw.source_files.map(normalizePath),
    evidence: raw.evidence?.map(ev => ({ ...ev, path: normalizePath(ev.path) })),
  }
  // 有订阅者说明 ChatPanel 正挂载在本会话上，它会即时落库；否则 store 代为落库。
  // 订阅者存在时 notify 同步触发其回调，不存在「回调前卸载」窗口。
  const hasSubscriber = (listeners.get(sessionId)?.size ?? 0) > 0
  let persisted = false
  if (!hasSubscriber && !e.cancelled) {
    // Don't let a stuck session write keep every future mount in "persisting".
    persisted = await Promise.race([
      persistDone(sessionId, e),
      new Promise<boolean>(resolve => setTimeout(() => resolve(false), 10_000)),
    ])
  }
  dones.set(sessionId, persisted ? { ...e, persisted: true } : e)
  streams.delete(sessionId)
  notify(sessionId)
}

function notify(sessionId: string) {
  const set = listeners.get(sessionId)
  if (!set) return
  for (const fn of set) {
    try {
      fn()
    } catch (err) {
      // One broken listener must not prevent the rest from clearing the
      // streaming state (and must not keep `active` alive).
      console.error('[streamStore] listener failed:', err)
    }
  }
}

function ensureStarted(): Promise<void> {
  if (!started) {
    started = (async () => {
      await client.listen<{ session_id: string; delta: string; reasoning?: boolean }>('ai-chunk', e => {
        active.add(e.session_id)
        armWatchdog(e.session_id)
        const s = streams.get(e.session_id) ?? { text: '', reasoning: '' }
        if (e.reasoning) s.reasoning += e.delta
        else s.text += e.delta
        streams.set(e.session_id, s)
        notify(e.session_id)
      })
      await client.listen<AiDonePayload>('ai-done', e => {
        void handleDone(e)
      })
      await client.listen<AnswerVerifyPayload>('answer_verify', e => {
        verifies.set(e.session_id, e)
        notify(e.session_id)
      })
    })().catch(err => {
      console.error('[streamStore] listen failed:', err)
    })
  }
  return started
}

export function subscribeAiStream(sessionId: string, onChange: () => void): () => void {
  void ensureStarted()
  const set = listeners.get(sessionId) ?? new Set<() => void>()
  set.add(onChange)
  listeners.set(sessionId, set)
  return () => {
    const cur = listeners.get(sessionId)
    if (!cur) return
    cur.delete(onChange)
    if (cur.size === 0) listeners.delete(sessionId)
  }
}

export function markStreamStarted(sessionId: string): void {
  // 新一轮开始 ⇒ 上一轮的核验结论作废，否则答案已经换掉、徽标还挂在旧结果上。
  verifies.delete(sessionId)
  void ensureStarted()
  active.add(sessionId)
  armWatchdog(sessionId)
}

export function markStreamEnded(sessionId: string): void {
  active.delete(sessionId)
}

export function isStreamInFlight(sessionId: string): boolean {
  return active.has(sessionId)
}

export function getStreamText(sessionId: string): string {
  return streams.get(sessionId)?.text ?? ''
}

export function getStreamReasoning(sessionId: string): string {
  return streams.get(sessionId)?.reasoning ?? ''
}

export function hasAiDone(sessionId: string): boolean {
  return dones.has(sessionId)
}

export function takeAiDone(sessionId: string): AiDonePayload | null {
  const p = dones.get(sessionId)
  if (p) dones.delete(sessionId)
  return p ?? null
}
