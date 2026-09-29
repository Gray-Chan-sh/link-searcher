import type { AiDonePayload, ChatSession } from '../api/files'

/** i18n 翻译函数签名（组件内 `t` 与模块级 `translate` 均满足）。 */
export type TranslateFn = (key: string, params?: Record<string, string | number>) => string

/** 把毫秒耗时格式化为「x分y秒」/「y秒」。 */
export function fmtTook(ms: number): string {
  const s = Math.round(ms / 1000)
  const m = Math.floor(s / 60)
  const ss = s % 60
  return m > 0 ? `${m}分${ss}秒` : `${ss}秒`
}

/**
 * 把一轮 `ai-done` 结果合并进会话：追加助手消息、来源与 per-turn 证据，
 * 并清空 pending 状态。纯函数，供两条路径共用：
 *  - ChatPanel（用户就在聊天页）收到事件后即时更新 UI 并落库；
 *  - streamStore（用户已切走、无订阅者）代为落库，避免回答随组件卸载丢失。
 */
export function buildSessionFromDone(
  cur: ChatSession,
  p: AiDonePayload,
  t: TranslateFn,
): ChatSession {
  const took = p.took_ms > 0 ? `\n\n⏱ ${fmtTook(p.took_ms)}` : ''
  // 网关偶发返回空流（content_chars=0）：显式错误而非静默"没有回答"
  const body = p.full_text.trim() ? p.full_text : `❌ ${t('err_empty_response')}`
  const sourcesPatch = p.source_ids.length > 0
    ? { source_ids: p.source_ids, source_files: p.source_files }
    : {}
  const userTurns = cur.messages.filter(m => m.role === 'user').length
  const perTurnPatch = {
    per_turn_evidence: [...(cur.per_turn_evidence ?? []), {
      turn_index: userTurns - 1,
      file_ids: p.source_ids,
      items: p.evidence ?? [],
      trace_id: p.trace_id ?? '',
      took_ms: p.took_ms,
      llm_model: p.llm_model ?? '',
      embedding_model: p.embedding_model ?? '',
      search_query: p.search_query ?? '',
      search_terms: p.search_terms ?? [],
      clarify_candidates: p.clarify_candidates ?? [],
      clarify_slots: p.clarify_slots ?? [],
      clarify_blocking: p.clarify_blocking ?? false,
      hits: p.hits ?? 0,
    }],
  }
  return {
    ...cur,
    messages: [...cur.messages, { role: 'assistant', content: body + took }],
    ...sourcesPatch,
    ...perTurnPatch,
    pending_query: null,
    pending_started_at: null,
  }
}
