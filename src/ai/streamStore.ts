import * as client from '../api/client'
import { normalizePath } from '../utils/normalizePath'
import { loadChatSession, saveChatSession, type AiDonePayload } from '../api/files'
import { translate } from '../i18n'
import { buildSessionFromDone } from './applyDone'

type StreamState = { text: string; reasoning: string }

const active = new Set<string>()
const streams = new Map<string, StreamState>()
const dones = new Map<string, AiDonePayload>()
const listeners = new Map<string, Set<() => void>>()
// 正在由 store 代为落库的会话：期间挂载的 ChatPanel 不得清 pending（会同文件竞争写）
const persisting = new Set<string>()
let started: Promise<void> | null = null

export function isPersisting(sessionId: string): boolean {
  return persisting.has(sessionId)
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
    persisted = await persistDone(sessionId, e)
  }
  dones.set(sessionId, persisted ? { ...e, persisted: true } : e)
  streams.delete(sessionId)
  notify(sessionId)
}

function notify(sessionId: string) {
  const set = listeners.get(sessionId)
  if (!set) return
  for (const fn of set) fn()
}

function ensureStarted(): Promise<void> {
  if (!started) {
    started = (async () => {
      await client.listen<{ session_id: string; delta: string; reasoning?: boolean }>('ai-chunk', e => {
        active.add(e.session_id)
        const s = streams.get(e.session_id) ?? { text: '', reasoning: '' }
        if (e.reasoning) s.reasoning += e.delta
        else s.text += e.delta
        streams.set(e.session_id, s)
        notify(e.session_id)
      })
      await client.listen<AiDonePayload>('ai-done', e => {
        void handleDone(e)
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
  void ensureStarted()
  active.add(sessionId)
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
