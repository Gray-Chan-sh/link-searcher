import { useState, useEffect } from 'react'
import { useI18n } from '../i18n'
import { getTurnAiEvents, type AiEvent } from '../api/files'

interface Props {
  sessionId: string
  turnNumber: number
}

const EVENT_ICONS: Record<string, string> = {
  query_rewrite: '✏️',
  scope_resolved: '📁',
  retrieval: '🔍',
  context_assembled: '📋',
  llm_call: '🤖',
  turn_complete: '✅',
}

const EVENT_KEYS: Record<string, string> = {
  query_rewrite: 'ai_event_query_rewrite',
  scope_resolved: 'ai_event_scope_resolved',
  retrieval: 'ai_event_retrieval',
  context_assembled: 'ai_event_context_assembled',
  llm_call: 'ai_event_llm_call',
  turn_complete: 'ai_event_turn_complete',
}

function formatPayload(type: string, payload: Record<string, unknown>): string {
  switch (type) {
    case 'query_rewrite': {
      const p = payload as { original?: string; rewritten?: string; was_rewritten?: boolean; rewrite_method?: string }
      if (!p.was_rewritten) return `${p.original}`
      return `${p.original} → ${p.rewritten} (${p.rewrite_method})`
    }
    case 'scope_resolved': {
      const p = payload as { dir_ids_count?: number; path_prefixes?: string[]; mention_files_count?: number; ext_filter?: string[] | null; date_from?: number | null; date_to?: number | null }
      const parts: string[] = []
      if (p.dir_ids_count) parts.push(`${p.dir_ids_count} dirs`)
      if (p.path_prefixes?.length) parts.push(p.path_prefixes.join(', '))
      if (p.mention_files_count) parts.push(`${p.mention_files_count} mentions`)
      if (p.ext_filter?.length) parts.push(`ext: ${p.ext_filter.join(',')}`)
      return parts.join(' · ') || '(all)'
    }
    case 'retrieval': {
      const p = payload as { search_query?: string; total_matches?: number; merged_hits?: number; semantic_fused?: boolean; from_history_count?: number }
      const parts = [`q="${p.search_query ?? ''}"`, `matched=${p.total_matches ?? p.merged_hits ?? 0}`]
      if (p.semantic_fused) parts.push('semantic')
      if (p.from_history_count) parts.push(`history=${p.from_history_count}`)
      return parts.join(' · ')
    }
    case 'context_assembled': {
      const p = payload as { material_count?: number; total_chars?: number; strict_docs?: boolean }
      return `${p.material_count} materials, ${p.total_chars} chars${p.strict_docs ? ' (strict)' : ''}`
    }
    case 'llm_call': {
      const p = payload as { model_id?: string; system_prompt_chars?: number; user_msg_chars?: number; streaming?: boolean }
      return `${p.model_id} · sys=${p.system_prompt_chars} user=${p.user_msg_chars}${p.streaming ? ' (stream)' : ''}`
    }
    case 'turn_complete': {
      const p = payload as { took_ms?: number; cancelled?: boolean; answer_chars?: number; source_count?: number; evidence_count?: number }
      if (p.cancelled) return 'cancelled'
      return `${p.took_ms}ms · ${p.answer_chars} chars · ${p.source_count} sources`
    }
    default:
      return JSON.stringify(payload)
  }
}

export default function AiEventTimeline({ sessionId, turnNumber }: Props) {
  const { t } = useI18n()
  const [events, setEvents] = useState<AiEvent[]>([])
  const [loading, setLoading] = useState(false)

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    getTurnAiEvents(sessionId, turnNumber).then(e => {
      if (!cancelled) setEvents(e)
    }).catch(() => {}).finally(() => { if (!cancelled) setLoading(false) })
    return () => { cancelled = true }
  }, [sessionId, turnNumber])

  if (loading) return <div className="text-xs text-gray-400 py-1">loading…</div>
  if (!events.length) return null

  return (
    <div className="mt-1 space-y-0.5">
      {events.map(ev => (
        <div key={ev.id} className="flex items-start gap-1.5 text-xs text-gray-500 dark:text-gray-400">
          <span className="shrink-0 mt-px">{EVENT_ICONS[ev.event_type] ?? '•'}</span>
          <span className="font-medium shrink-0">{t(EVENT_KEYS[ev.event_type] ?? ev.event_type)}</span>
          <span className="truncate">{formatPayload(ev.event_type, ev.payload)}</span>
        </div>
      ))}
    </div>
  )
}
