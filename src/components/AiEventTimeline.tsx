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
  sufficiency_check: '⚖️',
  progressive_round: '🔁',
  citation_check: '🔗',
  retrieval_fingerprint: '🧭',
  retrieval_anomaly: '⚠️',
  context_assembled: '📋',
  prompt_snapshot: '📜',
  llm_call: '🤖',
  turn_complete: '✅',
}

const EVENT_KEYS: Record<string, string> = {
  query_rewrite: 'ai_event_query_rewrite',
  scope_resolved: 'ai_event_scope_resolved',
  retrieval: 'ai_event_retrieval',
  sufficiency_check: 'ai_event_sufficiency_check',
  progressive_round: 'ai_event_progressive_round',
  citation_check: 'ai_event_citation_check',
  retrieval_fingerprint: 'ai_event_retrieval_fingerprint',
  retrieval_anomaly: 'ai_event_retrieval_anomaly',
  context_assembled: 'ai_event_context_assembled',
  prompt_snapshot: 'ai_event_prompt_snapshot',
  llm_call: 'ai_event_llm_call',
  turn_complete: 'ai_event_turn_complete',
}

function formatPayload(type: string, payload: Record<string, unknown>): string {
  switch (type) {
    case 'query_rewrite': {
      const p = payload as { original?: string; rewritten?: string; was_rewritten?: boolean; rewrite_method?: string; basis?: string }
      const base = !p.was_rewritten ? `${p.original}` : `${p.original} → ${p.rewritten} (${p.rewrite_method})`
      return p.basis ? `${base} · basis=${p.basis}` : base
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
    case 'sufficiency_check': {
      const p = payload as {
        round?: number | string
        phase?: string
        hit_count?: number
        sufficient?: boolean
        reasons?: unknown[]
        missing_entities?: string[]
        missing_slots?: string[]
      }
      const parts = [`hits=${p.hit_count ?? 0}`, p.sufficient ? 'sufficient' : 'insufficient']
      if (p.phase) parts.push(p.phase)
      if (p.reasons?.length) parts.push(`reasons=${p.reasons.length}`)
      if (p.missing_entities?.length) parts.push(`missing=${p.missing_entities.join(',')}`)
      if (p.missing_slots?.length) parts.push(`slots=${p.missing_slots.join(',')}`)
      return parts.join(' · ')
    }
    case 'progressive_round': {
      const p = payload as {
        round?: number
        phase?: string
        queries?: string[]
        added?: number
        hit_count?: number
      }
      const parts = [`round=${p.round ?? 1}`, `added=${p.added ?? 0}`, `hits=${p.hit_count ?? 0}`]
      if (p.phase) parts.push(p.phase)
      if (p.queries?.length) parts.push(`q=${p.queries.length}`)
      return parts.join(' · ')
    }
    case 'citation_check': {
      const p = payload as {
        citations?: number
        invalid_citations?: number
        visible_materials?: number
        cancelled?: boolean
      }
      if (p.cancelled) return 'cancelled'
      return `citations=${p.citations ?? 0} · invalid=${p.invalid_citations ?? 0} · visible=${p.visible_materials ?? 0}`
    }
    case 'retrieval_fingerprint': {
      const p = payload as {
        channels?: { bm25?: number; vector?: number; chunk?: number; path?: number }
        pool?: number
        injected?: number
      }
      const c = p.channels ?? {}
      return `bm25=${c.bm25 ?? 0} · vec=${c.vector ?? 0} · chunk=${c.chunk ?? 0} · path=${c.path ?? 0} · pool=${p.pool ?? 0} → ${p.injected ?? 0}`
    }
    case 'retrieval_anomaly': {
      const p = payload as { channel?: string; returned?: number; matched_total?: number }
      return `${p.channel}: ${p.returned ?? 0} of ${p.matched_total ?? 0}`
    }
    case 'context_assembled': {
      const p = payload as { material_count?: number; total_chars?: number; strict_docs?: boolean }
      return `${p.material_count} materials, ${p.total_chars} chars${p.strict_docs ? ' (strict)' : ''}`
    }
    case 'prompt_snapshot': {
      const p = payload as { system_chars?: number; user_chars?: number; system_digest?: string }
      return `system ${p.system_chars ?? 0} chars · user ${p.user_chars ?? 0} chars · #${p.system_digest ?? ''}`
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
      {events.map(ev => {
        if (ev.event_type === 'prompt_snapshot') {
          const p = ev.payload as {
            system_chars?: number
            system_head?: string
            system_digest?: string
            user_chars?: number
            user_head?: string
            user_tail?: string
            user_digest?: string
          }
          return (
            <details key={ev.id} className="text-xs text-gray-500 dark:text-gray-400">
              <summary className="cursor-pointer flex items-start gap-1.5">
                <span className="shrink-0 mt-px">{EVENT_ICONS.prompt_snapshot}</span>
                <span className="font-medium shrink-0">{t('ai_event_prompt_snapshot')}</span>
                <span className="truncate">{formatPayload(ev.event_type, ev.payload)}</span>
              </summary>
              <div className="mt-1 ml-5 space-y-2">
                <div>
                  <div className="font-medium text-gray-600 dark:text-gray-300">
                    system · #{p.system_digest ?? ''}
                  </div>
                  <pre className="mt-0.5 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded bg-gray-50 dark:bg-gray-800/60 p-2 text-[11px] leading-relaxed">
                    {p.system_head}
                  </pre>
                </div>
                <div>
                  <div className="font-medium text-gray-600 dark:text-gray-300">
                    user · #{p.user_digest ?? ''}
                  </div>
                  <pre className="mt-0.5 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded bg-gray-50 dark:bg-gray-800/60 p-2 text-[11px] leading-relaxed">
                    {p.user_head}
                  </pre>
                  {p.user_tail && p.user_tail !== p.user_head && (
                    <pre className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-words rounded bg-gray-50 dark:bg-gray-800/60 p-2 text-[11px] leading-relaxed">
                      {p.user_tail}
                    </pre>
                  )}
                </div>
              </div>
            </details>
          )
        }
        return (
          <div key={ev.id} className="flex items-start gap-1.5 text-xs text-gray-500 dark:text-gray-400">
            <span className="shrink-0 mt-px">{EVENT_ICONS[ev.event_type] ?? '•'}</span>
            <span className="font-medium shrink-0">{t(EVENT_KEYS[ev.event_type] ?? ev.event_type)}</span>
            <span className="truncate">{formatPayload(ev.event_type, ev.payload)}</span>
          </div>
        )
      })}
    </div>
  )
}
