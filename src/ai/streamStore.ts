import * as client from '../api/client'
import { normalizePath } from '../utils/normalizePath'
import type { AiDonePayload } from '../api/files'

type StreamState = { text: string; reasoning: string }

const active = new Set<string>()
const streams = new Map<string, StreamState>()
const dones = new Map<string, AiDonePayload>()
const listeners = new Map<string, Set<() => void>>()
let started: Promise<void> | null = null

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
        active.delete(e.session_id)
        dones.set(e.session_id, {
          ...e,
          source_files: e.source_files.map(normalizePath),
          evidence: e.evidence?.map(ev => ({ ...ev, path: normalizePath(ev.path) })),
        })
        streams.delete(e.session_id)
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
