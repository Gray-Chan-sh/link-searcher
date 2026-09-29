import { describe, it, expect } from 'vitest'
import { buildSessionFromDone, fmtTook } from '../applyDone'
import type { AiDonePayload, ChatSession } from '../../api/files'

const t = (key: string) => (key === 'err_empty_response' ? 'AI 未返回任何内容，请重试' : key)

function baseSession(): ChatSession {
  return {
    id: 's1',
    title: '会话',
    created_at: 0,
    updated_at: 0,
    messages: [{ role: 'user', content: '问题' }],
    source_ids: [],
    source_files: [],
    per_turn_evidence: [],
    retrieval_scope: [],
    strict_docs: true,
    full_recall: false,
    pending_query: '问题',
    pending_started_at: 123,
  }
}

function done(over: Partial<AiDonePayload> = {}): AiDonePayload {
  return {
    session_id: 's1',
    full_text: '回答',
    took_ms: 0,
    cancelled: false,
    source_ids: [],
    source_files: [],
    ...over,
  }
}

describe('fmtTook', () => {
  it('formats seconds and minutes', () => {
    expect(fmtTook(3000)).toBe('3秒')
    expect(fmtTook(65000)).toBe('1分5秒')
  })
})

describe('buildSessionFromDone', () => {
  it('appends the assistant answer and clears pending', () => {
    const s = buildSessionFromDone(baseSession(), done(), t)
    expect(s.messages).toHaveLength(2)
    expect(s.messages[1]).toEqual({ role: 'assistant', content: '回答' })
    expect(s.pending_query).toBeNull()
    expect(s.pending_started_at).toBeNull()
  })

  it('appends took suffix, sources and per-turn evidence', () => {
    const s = buildSessionFromDone(baseSession(), done({
      took_ms: 65000,
      source_ids: ['f1'],
      source_files: ['a.txt'],
      evidence: [{ file_id: 'f1', path: 'a.txt', snippet: 'x' }],
      search_query: 'q',
      hits: 3,
    }), t)
    expect(s.messages[1]!.content).toContain('⏱ 1分5秒')
    expect(s.source_ids).toEqual(['f1'])
    expect(s.source_files).toEqual(['a.txt'])
    const ev = s.per_turn_evidence![0]!
    expect(ev.turn_index).toBe(0)
    expect(ev.file_ids).toEqual(['f1'])
    expect(ev.items).toHaveLength(1)
    expect(ev.search_query).toBe('q')
    expect(ev.hits).toBe(3)
  })

  it('uses the localized error marker for an empty response', () => {
    const s = buildSessionFromDone(baseSession(), done({ full_text: '' }), t)
    expect(s.messages[1]!.content).toBe('❌ AI 未返回任何内容，请重试')
  })

  it('computes turn_index from existing user turns', () => {
    const cur = baseSession()
    cur.messages = [
      { role: 'user', content: 'q1' },
      { role: 'assistant', content: 'a1' },
      { role: 'user', content: 'q2' },
    ]
    const s = buildSessionFromDone(cur, done(), t)
    expect(s.per_turn_evidence![0]!.turn_index).toBe(1)
  })
})
