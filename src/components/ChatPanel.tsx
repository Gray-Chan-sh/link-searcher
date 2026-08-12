import { useState, useRef, useEffect, useCallback, useMemo } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { useI18n } from '../i18n'
import { LoadingSpinner } from '../icons'
import { smartSearch, conversationAsk, cancelAiRequest, smartSearchStream, conversationAskStream, listenAiStream, openFile, searchFilePaths, type ChatMessage, type ChatSession, type TurnScope } from '../api/files'
import MentionPicker from './MentionPicker'

interface ChatPanelProps {
  llmEnabled: boolean
  session: ChatSession | null
  onSessionChange: (session: ChatSession | null) => void
  /** 父组件（树状浏览器）请求插入 `@path` 到输入框；插完调 onMentionConsumed */
  pendingMention?: string | null
  onMentionConsumed?: () => void
}

export default function ChatPanel({ llmEnabled, session, onSessionChange, pendingMention, onMentionConsumed }: ChatPanelProps) {
  const { t } = useI18n()
  const [input, setInput] = useState('')
  const [showSources, setShowSources] = useState(false)
  const [clockNow, setClockNow] = useState(() => Date.now())
  // @mention 选择器状态
  const [mentionQuery, setMentionQuery] = useState('')
  const [mentionPos, setMentionPos] = useState<{ left: number; top: number } | null>(null)
  // 当前输入中 @mention 的 chips 预览
  const [mentionChips, setMentionChips] = useState<{ isFile: boolean; path: string }[]>([])
  const inputRef = useRef<HTMLInputElement>(null)
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

  // 消费父组件（树状浏览器）发来的待插入路径：追加 `@路径` 到输入框并更新 chips
  const insertMention = useCallback((path: string) => {
    setInput(prev => `${prev}${prev ? ' ' : ''}@${path} `)
    setMentionChips(prev => {
      const isFile = /\.\w{1,6}$/.test(path)
      return prev.some(c => c.path === path)
        ? prev
        : [...prev, { isFile, path }]
    })
    inputRef.current?.focus()
  }, [])

  useEffect(() => {
    if (!pendingMention) return
    insertMention(pendingMention)
    onMentionConsumed?.()
  }, [pendingMention, onMentionConsumed, insertMention])

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
          ? { per_turn_evidence: [...(cur.per_turn_evidence ?? []), { turn_index: userTurns - 1, file_ids: p.source_ids, items: p.evidence ?? [] }] }
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

  // 每条助手消息对应的本轮检索证据（按 turn_index 匹配）。
  const evidenceFor = (msgIndex: number) => {
    const userBefore = messages.slice(0, msgIndex).filter(m => m.role === 'user').length
    const turn = userBefore - 1
    return (session?.per_turn_evidence ?? []).find(e => e.turn_index === turn)?.items ?? []
  }
  const fmtScore = (v: number | null | undefined) => (v == null ? '—' : v.toFixed(2))

  // @mention 选择器：检测输入中 @ 并提取其后查询文本
  const handleInputChange = useCallback((value: string) => {
    setInput(value)
    // 实时解析 @mention 更新 chips 预览
    const chips: { isFile: boolean; path: string }[] = []
    const re = /@([^\s，。？！；:、,?:;]+)/g
    let m: RegExpExecArray | null
    while ((m = re.exec(value)) !== null) {
      const path = m[1].trim()
      if (!path) continue
      const isFile = /\.\w{1,6}$/.test(path)
      if (!chips.some(c => c.path === path)) chips.push({ isFile, path })
    }
    setMentionChips(chips)
    // 检测 @ 触发选择器
    const lastAt = value.lastIndexOf('@')
    if (lastAt !== -1) {
      // 检查 @ 后面是否还有空格（有空格则关闭选择器）
      const afterAt = value.slice(lastAt + 1)
      const hasSpace = afterAt.includes(' ')
      if (hasSpace) {
        setMentionQuery('')
        setMentionPos(null)
      } else {
        setMentionQuery(afterAt)
        // 计算 popup 位置：基于输入框位置
        if (inputRef.current) {
          const rect = inputRef.current.getBoundingClientRect()
          // 粗略估算光标位置（按字符数比例）
          const charPct = (value.length - afterAt.length) / Math.max(value.length, 1)
          setMentionPos({
            left: rect.left + charPct * rect.width,
            top: rect.top - 10,
          })
        }
      }
    } else {
      setMentionQuery('')
      setMentionPos(null)
    }
  }, [])

  const handleMentionSelect = useCallback((path: string) => {
    const lastAt = input.lastIndexOf('@')
    if (lastAt !== -1) {
      const before = input.slice(0, lastAt)
      const after = input.slice(lastAt + 1)
      const spaceIdx = after.indexOf(' ')
      const newInput = spaceIdx === -1
        ? `${before}@${path} `
        : `${before}@${path}${after.slice(spaceIdx)}`
      setInput(newInput)
      // 更新 chips
      const isFile = /\.\w{1,6}$/.test(path)
      setMentionChips(p => p.some(c => c.path === path) ? p : [...p, { isFile, path }])
    }
    setMentionQuery('')
    setMentionPos(null)
    inputRef.current?.focus()
  }, [input])

  const escapeRegex = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const handleChipRemove = useCallback((path: string) => {
    setInput(prev => prev.replace(new RegExp(`@${escapeRegex(path)}`, 'g'), '').trim())
    setMentionChips(prev => prev.filter(c => c.path !== path))
  }, [])

  // 解析输入文本中的 @mention token，提取文件/目录路径，生成 TurnScope。
  const parseScope = useCallback((text: string): TurnScope => {
    const mentionFiles: string[] = []
    const mentionDirs: string[] = []
    // 匹配 @ 开头直至下一个空格或标点（，。？！；:、）或行尾
    const re = /@([^\s，。？！；:、,?:;]+)/g
    let m: RegExpExecArray | null
    while ((m = re.exec(text)) !== null) {
      const path = m[1].trim()
      if (!path) continue
      // 以 / 结尾或含子目录标记 → 目录；否则 → 文件
      // 简单启发：路径不含扩展名且无点号 → 目录
      const hasExt = /\.\w{1,6}$/.test(path)
      if (hasExt) {
        if (!mentionFiles.includes(path)) mentionFiles.push(path)
      } else {
        if (!mentionDirs.includes(path)) mentionDirs.push(path)
      }
    }
    return { mention_files: mentionFiles, mention_dirs: mentionDirs, inherit_from: [], conditions: [] }
  }, [])

  const handleSend = useCallback(async () => {
    const q = input.trim()
    if (!q || loading || !session) return
    // 解析 @mention 并净化输入文本（移除 @token 供发送）
    const scope = parseScope(q)
    const cleanQ = q.replace(/@[^\s，。？！；:、,?:;]+/g, '').trim() || q
    setInput('')
    const userMsg: ChatMessage = { role: 'user', content: cleanQ }
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
        await conversationAskStream([...messages, userMsg], sourceIds, session.id, scope)
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

  // LLM 未配置时仍渲染历史会话（只读回放：来源栏 + 消息 + 证据面板），
  // 仅输入区替换为"AI 服务未配置"提示——会话审计不依赖网关在线。
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
                : (
                  <>
                    <ReactMarkdown remarkPlugins={[remarkGfm]}>{m.content}</ReactMarkdown>
                    {evidenceFor(i).length > 0 && (
                      <details className="mt-2 text-xs text-gray-500 dark:text-gray-400">
                        <summary className="cursor-pointer select-none hover:text-purple-600 dark:hover:text-purple-300">
                          🔍 {t('evidence')}（{evidenceFor(i).length}）
                        </summary>
                        <ul className="mt-1 space-y-1">
                          {evidenceFor(i).map((ev, j) => (
                            <li key={`${ev.file_id}-${j}`} className="rounded bg-gray-50 dark:bg-gray-900/40 px-2 py-1">
                              <div className="flex items-center gap-1 flex-wrap">
                                <span className="font-medium text-gray-700 dark:text-gray-300 truncate max-w-[70%]">{ev.path}</span>
                                {ev.from_history && (
                                  <span className="px-1 rounded bg-gray-200 dark:bg-gray-700 text-gray-500 dark:text-gray-400">{t('evidence_from_history')}</span>
                                )}
                                {ev.rewritten && (
                                  <span className="px-1 rounded bg-amber-100 dark:bg-amber-900/40 text-amber-700 dark:text-amber-300" title={ev.rewritten_query ?? ''}>
                                    {t('evidence_rewritten')}
                                  </span>
                                )}
                              </div>
                              <div className="text-gray-400 dark:text-gray-500">
                                BM25 {fmtScore(ev.bm25_score)}
                                {ev.semantic_score != null && <> · 语义 {fmtScore(ev.semantic_score)}</>}
                                {ev.rrf_score != null && <> · RRF {fmtScore(ev.rrf_score)}</>}
                                {ev.rewritten_query && <span className="ml-1">→ {ev.rewritten_query}</span>}
                              </div>
                              {ev.snippet && <div className="truncate mt-0.5">{ev.snippet}</div>}
                            </li>
                          ))}
                        </ul>
                      </details>
                    )}
                  </>
                )
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

      {mentionChips.length > 0 && (
        <div className="px-4 py-1.5 border-t border-gray-200 dark:border-gray-800 flex flex-wrap gap-1">
          {mentionChips.map((chip, i) => (
            <span
              key={`${chip.path}-${i}`}
              className="inline-flex items-center gap-1 px-2 py-0.5 text-[10px] rounded-full bg-purple-50 dark:bg-purple-900/20 text-purple-700 dark:text-purple-300 border border-purple-200 dark:border-purple-800"
            >
              <span>{chip.isFile ? '📄' : '📁'}</span>
              <span className="truncate max-w-40">{chip.path}</span>
              <button
                type="button"
                onClick={() => handleChipRemove(chip.path)}
                className="ml-0.5 text-purple-400 hover:text-purple-600 dark:hover:text-purple-200"
              >
                ×
              </button>
            </span>
          ))}
        </div>
      )}

      {llmEnabled ? (
        <div className="relative px-4 py-2 border-t border-gray-200 dark:border-gray-800 flex gap-2">
          <MentionPicker
            query={mentionQuery}
            position={mentionPos}
            onSelect={handleMentionSelect}
            onClose={() => { setMentionQuery(''); setMentionPos(null) }}
          />
          <input
            ref={inputRef}
            type="text"
            value={input}
            onChange={e => handleInputChange(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleSend() } }}
            onDragOver={e => e.preventDefault()}
            onDrop={e => { e.preventDefault(); const p = e.dataTransfer.getData('text/plain'); if (p) insertMention(p) }}
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
      ) : (
        <div className="px-4 py-2 border-t border-gray-200 dark:border-gray-800 text-center text-xs text-gray-400">
          {t('ai_llm_unavailable')}
        </div>
      )}
    </div>
  )
}
