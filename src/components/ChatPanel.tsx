import { useState, useRef, useEffect, useCallback, useMemo } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { useI18n } from '../i18n'
import { LoadingSpinner } from '../icons'
import { smartSearch, conversationAsk, cancelAiRequest, smartSearchStream, conversationAskStream, listenAiStream, openFile, type ChatMessage, type ChatSession } from '../api/files'

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
  // 流式输出缓冲：显示在"思考中"下方，done 后并入完整消息。
  const [streaming, setStreaming] = useState<{ sessionId: string; text: string } | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  // 在途请求标识：取消或新请求会递增它，旧请求的迟到响应据此丢弃。
  const latestReqIdRef = useRef(0)
  // 自发起防护：handleSend 设置 pending 后，恢复 effect 不应把自己刚发起
  // 的请求当作"残留 pending"再重跑一次（否则每轮追问都会并发两个请求）。
  const skipResumeRef = useRef(false)
  // 事件回调需要"最新"会话值（组件卸载/会话切换后仍可能收到迟到事件）。
  const sessionRef = useRef(session)
  const messagesRef = useRef<ChatMessage[]>([])
  const loadingRef = useRef(false)

  const messages = session?.messages ?? []
  messagesRef.current = messages
  const sourceIds = session?.source_ids ?? []
  const sourceFiles = session?.source_files ?? []
  // loading 由会话持久字段驱动 —— 切页/切会话再切回可恢复"思考中"。
  const pendingStartedAt = session?.pending_started_at ?? null
  const loading = pendingStartedAt != null
  loadingRef.current = loading
  useEffect(() => { sessionRef.current = session }, [session])

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
    setStreaming(null)
    patchSession({ pending_query: null, pending_started_at: null, messages })
  }, [loading, patchSession, messages])

  // 流式事件监听：增量文本实时显示；done 事件写回完整回答 + 响应耗时。
  useEffect(() => {
    if (!session?.id) return
    let unlisten: (() => void) | undefined
    let disposed = false
    const sessId = session.id
    listenAiStream(
      sessId,
      delta => setStreaming(s => ({ sessionId: sessId, text: (s?.sessionId === sessId ? s.text : '') + delta })),
      p => {
        if (disposed) return
        // pending 已被清除（取消/新请求）→ 迟到的完成事件直接丢弃。
        if (!loadingRef.current) { setStreaming(null); return }
        setStreaming(null)
        if (p.cancelled) return
        const cur = sessionRef.current
        if (!cur) return
        const took = p.took_ms > 0 ? `\n\n⏱ ${fmtTook(p.took_ms)}` : ''
        const sourcesPatch = p.source_ids.length > 0
          ? { source_ids: p.source_ids, source_files: p.source_files }
          : {}
        const userTurns = messagesRef.current.filter(m => m.role === 'user').length
        const perTurnPatch = p.source_ids.length > 0
          ? { per_turn_evidence: [...(cur.per_turn_evidence ?? []), { turn_index: userTurns - 1, file_ids: p.source_ids }] }
          : {}
        onSessionChange({
          ...cur,
          messages: [...messagesRef.current, { role: 'assistant', content: p.full_text + took }],
          ...sourcesPatch,
          ...perTurnPatch,
          pending_query: null,
          pending_started_at: null,
        })
      },
    ).then(fn => { if (disposed) { fn(); return } unlisten = fn })
      .catch(() => {})
    return () => { disposed = true; unlisten?.() }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session?.id])

  const fmtTook = (ms: number) => {
    const s = Math.round(ms / 1000)
    const m = Math.floor(s / 60)
    const ss = s % 60
    return m > 0 ? `${m}分${ss}秒` : `${ss}秒`
  }

  const handleSend = useCallback(async () => {
    const q = input.trim()
    if (!q || loading || !session) return
    setInput('')
    const userMsg: ChatMessage = { role: 'user', content: q }
    const reqId = ++latestReqIdRef.current
    const startedAt = Date.now()
    skipResumeRef.current = true
    patchSession({
      messages: [...messages, userMsg],
      pending_query: q,
      pending_started_at: startedAt,
    })
    setStreaming({ sessionId: session.id, text: '' })

    try {
      if (sourceIds.length === 0) {
        await smartSearchStream(q, session.id)
      } else {
        await conversationAskStream([...messages, userMsg], sourceIds, session.id)
      }
      // 命令成功返回后内容经 ai-chunk/ai-done 事件写入，无需在此处理。
    } catch (e) {
      if (latestReqIdRef.current !== reqId) return
      setStreaming(null)
      patchSession({
        messages: [...messages, userMsg, { role: 'assistant', content: `❌ ${errText(e)}` }],
        pending_query: null,
        pending_started_at: null,
      })
    }
  }, [input, loading, session, messages, sourceIds, patchSession])

  // 恢复挂起的请求：切页/切会话后返回时看到残留 pending，直接重跑该
  // 问题（若进程内原请求尚未结束，会重复消耗一次生成——可用性优先，
  // 后续再以流式替代）。
  useEffect(() => {
    if (!session?.pending_query) return
    // 本组件刚发起的请求（handleSend）不恢复——避免并发双请求。
    if (skipResumeRef.current) {
      skipResumeRef.current = false
      return
    }
    const q = session.pending_query
    const reqId = ++latestReqIdRef.current
    const base = session.messages
    ;(async () => {
      try {
        if (sourceIds.length === 0) {
          const res = await smartSearch(q)
          if (latestReqIdRef.current !== reqId) return
          patchSession({
            messages: [...base, { role: 'assistant', content: res.answer }],
            source_ids: res.source_ids,
            source_files: res.source_files,
            pending_query: null,
            pending_started_at: null,
          })
        } else {
          const answer = await conversationAsk(base, sourceIds)
          if (latestReqIdRef.current !== reqId) return
          patchSession({
            messages: [...base, { role: 'assistant', content: answer }],
            pending_query: null,
            pending_started_at: null,
          })
        }
      } catch (e) {
        if (latestReqIdRef.current !== reqId) return
        patchSession({
          messages: [...base, { role: 'assistant', content: `❌ ${errText(e)}` }],
          pending_query: null,
          pending_started_at: null,
        })
      }
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
        {streaming && streaming.sessionId === session?.id && streaming.text && (
          <div className="text-sm text-gray-600 dark:text-gray-300 whitespace-pre-wrap border-l-2 border-gray-200 dark:border-gray-700 pl-3">
            {streaming.text}
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
