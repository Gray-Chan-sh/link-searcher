import { useState, useRef, useEffect, useCallback, useMemo } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { useI18n } from '../i18n'
import { LoadingSpinner } from '../icons'
import { smartSearch, conversationAsk, cancelAiRequest, openFile, type ChatMessage, type SmartSearchResponse, type ChatSession } from '../api/files'

// 模块级活跃 AI 请求注册表（不随组件卸载消失）：页面/会话切换后
// 返回时挂接同一请求结果，避免向 LLM 网关重复发起请求。
interface ActiveAiResult {
  ok: boolean
  answer?: string
  error?: string
  sourceIds?: string[]
  sourceFiles?: string[]
}
interface ActiveAiRequest {
  result?: ActiveAiResult
  listeners: Set<(r: ActiveAiResult) => void>
}
const activeAiRequests = new Map<string, ActiveAiRequest>()

interface ChatPanelProps {
  llmEnabled: boolean
  session: ChatSession | null
  onSessionChange: (session: ChatSession | null) => void
}

export default function ChatPanel({ llmEnabled, session, onSessionChange }: ChatPanelProps) {
  const { t } = useI18n()
  const [input, setInput] = useState('')
  const [showSources, setShowSources] = useState(false)
  const [clockNow, setClockNow] = useState(() => Date.now())
  const scrollRef = useRef<HTMLDivElement>(null)
  // 在途请求标识：取消或新请求会递增它，旧请求的迟到响应据此丢弃。
  const latestReqIdRef = useRef(0)

  const messages = session?.messages ?? []
  const sourceIds = session?.source_ids ?? []
  const sourceFiles = session?.source_files ?? []
  // loading 由会话持久字段驱动 —— 切页/切会话再切回可恢复"思考中"。
  const pendingStartedAt = session?.pending_started_at ?? null
  const loading = pendingStartedAt != null

  const patchSession = useCallback((patch: Partial<ChatSession>) => {
    if (session) onSessionChange({ ...session, ...patch })
  }, [session, onSessionChange])

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: 'smooth' })
  }, [messages, loading])

  // 请求进行中每秒走表，驱动 "mm:ss" 计时。
  useEffect(() => {
    if (!loading) return
    setClockNow(Date.now())
    const it = setInterval(() => setClockNow(Date.now()), 1000)
    return () => clearInterval(it)
  }, [loading])

  const elapsedText = useMemo(() => {
    if (!pendingStartedAt) return ''
    const s = Math.max(0, Math.floor((clockNow - pendingStartedAt) / 1000))
    const mm = String(Math.floor(s / 60)).padStart(2, '0')
    const ss = String(s % 60).padStart(2, '0')
    return `${mm}:${ss}`
  }, [pendingStartedAt, clockNow])

  const errText = (e: unknown) =>
    e instanceof Error && e.message
      ? e.message
      : typeof e === 'string'
        ? e
        : typeof e === 'object' && e !== null && 'message' in e
          ? String((e as { message: unknown }).message)
          : String(e)

  const handleCancel = useCallback(async () => {
    if (!loading) return
    if (!confirm('确认取消当前回答？')) return
    cancelAiRequest().catch(() => {})
    latestReqIdRef.current += 1
    // 从注册表移除：切回时不再等待这个已被取消的请求。
    if (session?.id) activeAiRequests.delete(session.id)
    patchSession({ pending_query: null, pending_started_at: null, messages })
  }, [loading, patchSession, messages, session])

  const runRequest = useCallback(async (q: string, src: string[], history: ChatMessage[]): Promise<ActiveAiResult> => {
    try {
      if (src.length === 0) {
        const res: SmartSearchResponse = await smartSearch(q)
        return { ok: true, answer: res.answer, sourceIds: res.source_ids, sourceFiles: res.source_files }
      }
      const answer = await conversationAsk(history, src)
      return { ok: true, answer }
    } catch (e) {
      return { ok: false, error: errText(e) }
    }
  }, [])

  const handleSend = useCallback(async () => {
    const q = input.trim()
    if (!q || loading || !session) return
    setInput('')
    const userMsg: ChatMessage = { role: 'user', content: q }
    const reqId = ++latestReqIdRef.current
    const startedAt = Date.now()
    const history = messages
    patchSession({
      messages: [...history, userMsg],
      pending_query: q,
      pending_started_at: startedAt,
    })

    const req: ActiveAiRequest = { listeners: new Set() }
    req.listeners.add((result) => {
      if (latestReqIdRef.current !== reqId) return
      const base = [...history, userMsg]
      if (result.ok) {
        patchSession({
          messages: [...base, { role: 'assistant', content: result.answer ?? '' }],
          source_ids: result.sourceIds,
          source_files: result.sourceFiles,
          pending_query: null,
          pending_started_at: null,
        })
      } else {
        patchSession({
          messages: [...base, { role: 'assistant', content: `❌ ${result.error}` }],
          pending_query: null,
          pending_started_at: null,
        })
      }
    })
    activeAiRequests.set(session.id, req)
    void (async () => {
      const result = await runRequest(q, sourceIds, history)
      req.result = result
      activeAiRequests.delete(session.id)
      req.listeners.forEach(fn => fn())
    })()
  }, [input, loading, session, messages, sourceIds, patchSession, runRequest])

  // 恢复挂起的请求：加载到带 pending 的会话（切页/切会话后返回）。
  // 同一进程内原请求仍在注册表里 → 只挂接等待其结果（不重复请求 LLM）；
  // 仅当注册表无记录（如 app 重启）才重发。
  useEffect(() => {
    if (!session?.pending_query) return
    const base = session.messages
    const apply = (result: ActiveAiResult) => {
      if (result.ok) {
        patchSession({
          messages: [...base, { role: 'assistant', content: result.answer ?? '' }],
          source_ids: result.sourceIds,
          source_files: result.sourceFiles,
          pending_query: null,
          pending_started_at: null,
        })
      } else {
        patchSession({
          messages: [...base, { role: 'assistant', content: `❌ ${result.error}` }],
          pending_query: null,
          pending_started_at: null,
        })
      }
    }
    const existing = activeAiRequests.get(session.id)
    if (existing) {
      if (existing.result) { apply(existing.result); return }
      existing.listeners.add(apply)
      return () => { existing.listeners.delete(apply) }
    }
    // 原请求已不存在（进程重启 / 取消后残留）——重发，这是唯一重发场景。
    const reqId = ++latestReqIdRef.current
    void (async () => {
      const result = await runRequest(session.pending_query as string, sourceIds, base)
      if (latestReqIdRef.current !== reqId) return
      apply(result)
    })()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session?.id, session?.pending_query])

  if (!llmEnabled) {
    return (
      <div className="flex-1 flex items-center justify-center text-sm text-gray-400">
        {t('ai_llm_unavailable')}
      </div>
    )
  }

  return (
    <div className="flex-1 flex flex-col min-h-0">
      {sourceFiles.length > 0 && (
        <div className="px-4 py-2 text-xs text-gray-500 dark:text-gray-400 bg-purple-50 dark:bg-purple-900/10 border-b border-purple-100 dark:border-purple-800/30">
          <div className="flex items-center gap-2">
            <span className="text-purple-600 dark:text-purple-400">✦</span>
            <button
              onClick={() => setShowSources(v => !v)}
              className="hover:text-purple-600 dark:hover:text-purple-300"
            >
              {t('source_files', { n: sourceFiles.length })}
              <span className="ml-1 text-gray-400">{showSources ? '▲' : '▼'}</span>
            </button>
          </div>
          {showSources && (
            <div className="mt-2 space-y-1 max-h-40 overflow-y-auto">
              {sourceFiles.map((f, i) => (
                <button
                  key={`${f}-${i}`}
                  onClick={() => sourceIds[i] && openFile(sourceIds[i])}
                  className="block w-full text-left px-2 py-1 rounded hover:bg-purple-100 dark:hover:bg-purple-900/30 truncate text-purple-700 dark:text-purple-300 hover:underline"
                  title={f}
                >
                  📄 {f}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      <div ref={scrollRef} className="flex-1 overflow-y-auto px-4 py-3 space-y-3">
        {messages.length === 0 && (
          <div className="text-center text-sm text-gray-400 py-12">
            {t('chat_placeholder')}
          </div>
        )}
        {messages.map((m, i) => (
          <div key={i} className={`flex ${m.role === 'user' ? 'justify-end' : 'justify-start'}`}>
            <div className={`max-w-[85%] px-3 py-2 rounded-lg text-sm ${
              m.role === 'user'
                ? 'bg-blue-600 text-white whitespace-pre-wrap'
                : 'bg-gray-100 dark:bg-gray-800 text-gray-800 dark:text-gray-200 prose prose-sm max-w-full dark:prose-invert'
            }`}>
              {m.role === 'user'
                ? m.content
                : <ReactMarkdown remarkPlugins={[remarkGfm]}>{m.content}</ReactMarkdown>
              }
            </div>
          </div>
        ))}
        {loading && (
          <div className="flex items-center gap-2 text-sm text-gray-500">
            <LoadingSpinner className="size-3.5" />
            <span>{t('thinking')} {elapsedText}</span>
            <button
              onClick={handleCancel}
              className="ml-1 px-1.5 py-0.5 text-xs text-red-500 hover:text-red-600 hover:bg-red-50 dark:hover:bg-red-900/30 rounded transition-colors"
            >
              {t('cancel')}
            </button>
          </div>
        )}
      </div>

      <div className="px-4 py-2 border-t border-gray-200 dark:border-gray-800 flex gap-2">
        <input
          type="text"
          value={input}
          onChange={e => setInput(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleSend() } }}
          placeholder={sourceIds.length > 0 ? t('ask_followup') : t('ask_question')}
          disabled={loading}
          className="flex-1 px-3 py-1.5 text-xs border border-gray-200 dark:border-gray-700 rounded bg-gray-50 dark:bg-gray-800 text-gray-700 dark:text-gray-300 placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-purple-500 disabled:opacity-40"
        />
        <button
          onClick={handleSend}
          disabled={loading || !input.trim() || !session}
          className="px-3 py-1.5 text-xs font-medium text-white bg-purple-600 hover:bg-purple-700 rounded disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
        >
          {t('send')}
        </button>
      </div>
    </div>
  )
}
