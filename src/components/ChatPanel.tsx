import { useState, useRef, useEffect, useCallback, useMemo } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import remarkCjkFriendly from 'remark-cjk-friendly/parseOnly'
import { useNavigate } from 'react-router-dom'
import { useI18n } from '../i18n'
import { LoadingSpinner } from '../icons'
import { cancelAiRequest, conversationAskStream, listenAiStream, listenAiProgress, openFile, type ChatMessage, type ChatSession, type AiProgressPayload } from '../api/files'
import { mergeScopePrefixes } from '../utils/scopeMerge'
import { parseScope, type TurnScope } from '../utils/scopeParser'
import { translateErr } from '../utils/translateErr'
import MentionPicker from './MentionPicker'
import AiEventTimeline from './AiEventTimeline'

interface ChatPanelProps {
  llmEnabled: boolean
  session: ChatSession | null
  onSessionChange: (session: ChatSession | null) => void
  /** 父组件（树状浏览器）请求插入 `@path` 到输入框；插完调 onMentionConsumed */
  pendingMention?: string | null
  onMentionConsumed?: () => void
  /** /范围:全库 或 /范围:目录路径 —— 交给持有 dirs 数据的父组件解析为路径后更新会话范围 */
  onScopeAction?: (action: string) => void
}

// 请求进行中每秒走表，驱动 "mm:ss" 计时。独立小组件：只有计时文本 re-render，
// 避免整个 ChatPanel 每 1s 全量 diff（消息流、chips、mention 状态都被卷入）。
function ElapsedTimer({ startedAt }: { startedAt: number }) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    setNow(Date.now())
    const it = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(it)
  }, [startedAt])
  const s = Math.max(0, Math.floor((now - startedAt) / 1000))
  const mm = String(Math.floor(s / 60)).padStart(2, '0')
  const ss = String(s % 60).padStart(2, '0')
  return <span>{`${mm}:${ss}`}</span>
}

export default function ChatPanel({ llmEnabled, session, onSessionChange, pendingMention, onMentionConsumed, onScopeAction }: ChatPanelProps) {
  const { t } = useI18n()
  const navigate = useNavigate()
  const [input, setInput] = useState('')
  const [showSources, setShowSources] = useState(false)
  // @mention 选择器状态
  const [mentionQuery, setMentionQuery] = useState('')
  const [mentionPos, setMentionPos] = useState<{ left: number; top: number } | null>(null)
  // 当前输入中 @mention 的 chips 预览
  const [mentionChips, setMentionChips] = useState<{ isFile: boolean; path: string }[]>([])
  // 输入中 /命令 解析结果（/ext /date /模糊），实时显示可审计
  const [conditionChips, setConditionChips] = useState<TurnScope['conditions']>([])
  // 输入中 /范围: 解析出的动作预览（'clear' | 'dir:xxx' | null），打字即反馈
  const [scopeActionPreview, setScopeActionPreview] = useState<string | null>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  const [streaming, setStreaming] = useState<{ sessionId: string; text: string; reasoning: string } | null>(null)
  const [progress, setProgress] = useState<AiProgressPayload | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  // 流式自动跟随：用户主动上滚查看历史时暂停跟随，回到底部后恢复
  const stickToBottomRef = useRef(true)
  const handleScroll = useCallback(() => {
    const el = scrollRef.current
    if (!el) return
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80
    stickToBottomRef.current = nearBottom
  }, [])
  const scrollToBottom = useCallback((behavior: ScrollBehavior = 'auto') => {
    const el = scrollRef.current
    if (el && stickToBottomRef.current) el.scrollTo({ top: el.scrollHeight, behavior })
  }, [])
  // 在途请求标识：取消或新请求会递增它，旧请求的迟到响应据此丢弃。
  const latestReqIdRef = useRef(0)
  // 发送中防护：阻止 Enter + 按钮点击双重触发
  const sendingRef = useRef(false)
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

  // 会话切换/新建后，上一会话的 @mention chips、/命令 condition chips 与输入文本
  // 不得泄漏到新会话（否则新问题会错误引用旧会话材料）。仅在 id 变化时重置。
  useEffect(() => {
    setMentionChips([])
    setConditionChips([])
    setScopeActionPreview(null)
    setInput('')
  }, [session?.id])

  // 消费父组件（树状浏览器）发来的待插入路径：追加 `@路径` 到输入框并更新 chips
  const insertMention = useCallback((path: string) => {
    const isFile = /\.\w{1,6}$/.test(path)
    setMentionChips(prev => prev.some(c => c.path === path) ? prev : [...prev, { isFile, path }])
    inputRef.current?.focus()
  }, [])

  useEffect(() => {
    if (!pendingMention) return
    insertMention(pendingMention)
    onMentionConsumed?.()
  }, [pendingMention, onMentionConsumed, insertMention])

  const patchSession = useCallback((patch: Partial<ChatSession>) => {
    const cur = sessionRef.current ?? session
    if (cur) onSessionChange({ ...cur, ...patch })
  }, [session, onSessionChange])

  // 新消息/流式文本增长时跟随滚动（用户上滚则暂停，scrollToBottom 内部判断）
  useEffect(() => {
    scrollToBottom('smooth')
  }, [messages, loading, streaming?.text])

  const errText = (e: unknown) => {
    const raw =
      e instanceof Error && e.message
        ? e.message
        : typeof e === 'string'
          ? e
          : typeof e === 'object' && e !== null && 'message' in e
            ? String((e as { message: unknown }).message)
            : String(e)
    return translateErr(raw, t)
  }

  const handleCancel = useCallback(async () => {
    if (!loading) return
    cancelAiRequest().catch(() => {})
    latestReqIdRef.current += 1
    sendingRef.current = false
    const partialText = streaming && streaming.sessionId === session?.id && streaming.text.trim()
      ? streaming.text + '\n\n⏹ 已取消'
      : '⏹ 已取消'
    patchSession({
      pending_query: null,
      pending_started_at: null,
      messages: [...messages, { role: 'assistant', content: partialText }],
    })
    setStreaming(null)
    setProgress(null)
  }, [loading, patchSession, messages, streaming, session?.id])

  // 流式事件监听：增量文本实时显示；done 事件写回完整回答 + 响应耗时。
  useEffect(() => {
    if (!session?.id) return
    let unlisten: (() => void) | undefined
    let disposed = false
    const sessId = session.id
    listenAiStream(
      sessId,
      (delta, isReasoning) => {
        console.log('[AI-DEBUG] ai-chunk', { deltaLen: delta?.length, isReasoning })
        setProgress(null)
        if (isReasoning) {
          setStreaming(s => ({
            sessionId: sessId,
            text: s?.sessionId === sessId ? s.text : '',
            reasoning: (s?.sessionId === sessId ? s.reasoning : '') + delta,
          }))
        } else {
          setStreaming(s => ({
            sessionId: sessId,
            text: (s?.sessionId === sessId ? s.text : '') + delta,
            reasoning: s?.sessionId === sessId ? s.reasoning : '',
          }))
        }
      },
      p => {
        console.log('[AI-DEBUG] ai-done received', { sessionId: p.session_id, loadingRef: loadingRef.current, disposed, fullTextLen: p.full_text?.length, cancelled: p.cancelled })
        if (disposed) return
        if (!loadingRef.current) { console.warn('[AI-DEBUG] ai-done dropped: loadingRef is false'); setStreaming(null); setProgress(null); return }
        setStreaming(null)
        setProgress(null)
        if (p.cancelled) return
        const cur = sessionRef.current
        if (!cur) return
        const took = p.took_ms > 0 ? `\n\n⏱ ${fmtTook(p.took_ms)}` : ''
        // 网关偶发返回空流（content_chars=0）：显式错误而非静默"没有回答"
        const body = p.full_text.trim()
          ? p.full_text
          : `❌ ${t('err_empty_response')}`
        const sourcesPatch = p.source_ids.length > 0
          ? { source_ids: p.source_ids, source_files: p.source_files }
          : {}
        const userTurns = messagesRef.current.filter(m => m.role === 'user').length
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
            hits: p.hits ?? 0,
          }]
        }
        onSessionChange({
          ...cur,
          messages: [...messagesRef.current, { role: 'assistant', content: body + took }],
          ...sourcesPatch,
          ...perTurnPatch,
          pending_query: null,
          pending_started_at: null,
        })
      },
    ).then(fn => { if (disposed) { fn(); return } unlisten = fn })
      .catch(e => console.error('[ChatPanel] listenAiStream failed:', e))
    return () => { disposed = true; unlisten?.() }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session?.id])

  useEffect(() => {
    if (!session?.id) return
    let un: (() => void) | undefined
    let disposed = false
    listenAiProgress(session.id, p => {
      if (disposed) return
      setProgress(p)
    }).then(fn => { if (disposed) { fn(); return } un = fn })
      .catch(e => console.error('[ChatPanel] listenAiProgress failed:', e))
    return () => { disposed = true; un?.(); setProgress(null) }
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
  const turnNumberFor = (msgIndex: number) => {
    return messages.slice(0, msgIndex).filter(m => m.role === 'user').length - 1
  }
  const fmtScore = (v: number | null | undefined) => (v == null ? '—' : v.toFixed(2))

  // @mention 选择器：检测输入中 @ 并提取其后查询文本
  const handleInputChange = useCallback((value: string) => {
    setInput(value)
    // 实时解析 /命令 条件（/ext /date /模糊），chips 区可见
    const { scope, scopeAction } = parseScope(value)
    setConditionChips(scope.conditions)
    // /范围: 打字中预览：解析出的 scopeAction 立即反映（'clear'=全库 / 'dir:xxx'=目录）
    setScopeActionPreview(scopeAction)
    // @mention 不再从文本解析 — chips 是真实数据源
    // 检测 @ 触发选择器
    const lastAt = value.lastIndexOf('@')
    if (lastAt !== -1) {
      const afterAt = value.slice(lastAt + 1)
      const hasSpace = afterAt.includes(' ')
      if (hasSpace) {
        setMentionQuery('')
        setMentionPos(null)
      } else {
        setMentionQuery(afterAt)
        if (inputRef.current) {
          const rect = inputRef.current.getBoundingClientRect()
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
      // 移除 @ 及后续查询文本，仅保留前面的文字
      const before = input.slice(0, lastAt)
      setInput(before + ' ')
    }
    // 增加 chip（不写入输入文本）
    const isFile = /\.\w{1,6}$/.test(path)
    setMentionChips(p => p.some(c => c.path === path) ? p : [...p, { isFile, path }])
    setMentionQuery('')
    setMentionPos(null)
    inputRef.current?.focus()
  }, [input])

  const handleChipRemove = useCallback((path: string) => {
    setMentionChips(prev => prev.filter(c => c.path !== path))
  }, [])

  // 合并"本轮将生效"的检索范围（只读摘要，发送前实时反映）
  const effectiveScope = useMemo(() => {
    const parts: string[] = []
    // 文件/目录引用：chips 是本轮真实来源
    if (mentionChips.length > 0) {
      parts.push(mentionChips.map(c => c.path).join(', '))
    }
    // 会话级统一范围（跨轮累计的路径条目，合并后）
    const mergedScope = session?.retrieval_scope ? mergeScopePrefixes(session.retrieval_scope) : []
    if (mergedScope.length > 0) {
      parts.push(mergedScope.join(', '))
    }
    // 条件
    if (conditionChips.length > 0) {
      parts.push(conditionChips.map(c => `/${c.kind}:${c.value}`).join(' '))
    }
    return parts
  }, [mentionChips, session, conditionChips])

// 解析输入文本：/命令 与 chips（@mention 由 chips 管理，不再依赖文本解析）
  const handleSend = useCallback(async () => {
    if (sendingRef.current) return
    const q = input.trim()
    if (!q || loading || !session) return
    sendingRef.current = true
    // 解析 /命令（/ext /date /范围 /模糊），得到 scope + 净化后文本 + 范围动作
    const { scope, cleanText, scopeAction } = parseScope(q)
    // 将 chips 合并到 scope（chips 是真实数据源）
    for (const chip of mentionChips) {
      if (chip.isFile) {
        if (!scope.mention_files.includes(chip.path)) scope.mention_files.push(chip.path)
      } else {
        if (!scope.mention_dirs.includes(chip.path)) scope.mention_dirs.push(chip.path)
      }
    }
    // 处理挂起的文件树引用（直接加入 scope，不依赖 React 状态更新）
    if (pendingMention) {
      const isFile = /\.\w{1,6}$/.test(pendingMention)
      if (isFile) {
        if (!scope.mention_files.includes(pendingMention)) scope.mention_files.push(pendingMention)
      } else {
        if (!scope.mention_dirs.includes(pendingMention)) scope.mention_dirs.push(pendingMention)
      }
      insertMention(pendingMention)
      onMentionConsumed?.()
    }
    const cleanQ = cleanText || q
    // /范围:全库 或 /范围:目录 —— 交给父组件（AiChat 持有 dirs id 映射）
    if (scopeAction) {
      onScopeAction?.(scopeAction)
    }
    setInput('')
    setMentionChips([])
    setConditionChips([])
    setScopeActionPreview(null)
    // 构造用户消息：问题文本 + 引用标注（让用户看清引用了什么）
    let userContent = cleanQ
    const allRefs = [
      ...mentionChips.map(c => ({ isFile: c.isFile, path: c.path })),
      ...(pendingMention ? [{ isFile: /\.\w{1,6}$/.test(pendingMention), path: pendingMention }] : []),
    ]
    // 去重
    const seen = new Set<string>()
    const uniqueRefs = allRefs.filter(r => { const k = r.path; if (seen.has(k)) return false; seen.add(k); return true })
    if (uniqueRefs.length > 0) {
      const refs = uniqueRefs.map(r => `${r.isFile ? '📄' : '📁'} ${r.path}`).join('\n')
      userContent += `\n\n---\n引用:\n${refs}`
    }
    const userMsg: ChatMessage = { role: 'user', content: userContent }
    // 搜索用纯文本版本（不含引用标注，避免 BM25 解析 emoji 报错）
    const searchMsg: ChatMessage = { role: 'user', content: cleanQ }
    const reqId = ++latestReqIdRef.current
    const startedAt = Date.now()
    const userTurnsCount = messages.filter(m => m.role === 'user').length
    // 跨轮累计：本轮 @引用路径并入会话持久范围（父吞子去冗余，直到手动删除）
    const mergedScope = mergeScopePrefixes([
      ...(session.retrieval_scope ?? []),
      ...uniqueRefs.map(r => r.path),
    ])
    patchSession({
      messages: [...messages, userMsg],
      pending_query: q,
      pending_started_at: startedAt,
      retrieval_scope: mergedScope,
      // 记录本轮生效的范围快照（跨轮累计合并后），供导出追溯
      per_turn_scopes: [
        ...(session.per_turn_scopes ?? []),
        { turn_index: userTurnsCount, scope: mergedScope },
      ],
    })
    setStreaming({ sessionId: session.id, text: '', reasoning: '' })

    try {
      // ponytail: smart_search_stream bypasses scope/semantic/rewrite; always use conversation path
      await conversationAskStream([...messages, searchMsg], sourceIds, session.id, scope, mergedScope, session.strict_docs ?? true, session.full_recall ?? false)
      // 命令成功返回后内容经 ai-chunk/ai-done 事件写入，无需在此处理。
    } catch (e) {
      sendingRef.current = false
      if (latestReqIdRef.current !== reqId) return
      setStreaming(null)
      patchSession({
        messages: [...messages, userMsg, { role: 'assistant', content: `❌ ${errText(e)}` }],
        pending_query: null,
        pending_started_at: null,
      })
      return
    }
    sendingRef.current = false
  }, [input, loading, session, messages, sourceIds, patchSession, onSessionChange, mentionChips, pendingMention, onMentionConsumed, insertMention])

  // 依赖只能是 session.id：若含 pending_query，handleSend 一设置它就会立刻清掉
  // 刚写的 pending_started_at，使 loadingRef 恒为 false，ai-done 被守卫丢弃。
  useEffect(() => {
    if (!session?.pending_query) return
    patchSession({ pending_query: null, pending_started_at: null })
  }, [session?.id])

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

      <div ref={scrollRef} onScroll={handleScroll} className="flex-1 overflow-y-auto px-4 py-3 space-y-3">
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
                    <ReactMarkdown
                      remarkPlugins={[remarkGfm, remarkCjkFriendly]}
                      components={{
                        a: ({ href, children }) => {
                          if (href?.startsWith('#ref:')) {
                            const idx = parseInt(href.slice(5), 10)
                            const ev = evidenceFor(i)
                            if (idx >= 0 && idx < ev.length && ev[idx]!.path) {
                              return (
                                <span
                                  className="text-blue-600 dark:text-blue-400 cursor-pointer underline decoration-dotted hover:underline"
                                  onClick={() => navigate('/browse?path=' + encodeURIComponent(ev[idx]!.path))}
                                >{children}</span>
                              )
                            }
                          }
                          return <a href={href} target="_blank" rel="noopener noreferrer">{children}</a>
                        }
                      }}
                    >{m.content.replace(/\[(\d+)\](\[(\d+)\])*/g, (match) => {
                      const nums = match.match(/\d+/g) || []
                      const ev = evidenceFor(i)
                      const links = nums.map(n => {
                        const idx = parseInt(n, 10) - 1
                        if (idx >= 0 && idx < ev.length && ev[idx]!.path) {
                          return `[${n}](#ref:${idx})`
                        }
                        return `[${n}]`
                      })
                      return links.join('')
                    })}</ReactMarkdown>
                    {evidenceFor(i).length > 0 && (
                      <details className="mt-2 text-xs text-gray-500 dark:text-gray-400">
                        <summary className="cursor-pointer select-none hover:text-purple-600 dark:hover:text-purple-300">
                          🔍 {t('evidence')}（{evidenceFor(i).length}）
                        </summary>
<ul className="mt-1 space-y-1">
                           {evidenceFor(i).map((ev, j) => (
                             <li key={`${ev.file_id}-${j}`} className="group rounded bg-gray-50 dark:bg-gray-900/40 px-2 py-1 hover:bg-gray-100 dark:hover:bg-gray-800">
                               <div className="flex items-center gap-1 flex-wrap">
                                 <button
                                   onClick={(e) => { e.stopPropagation(); setMentionChips(prev => prev.some(c => c.path === ev.path) ? prev : [...prev, { isFile: !ev.path.endsWith('/'), path: ev.path }]) }}
                                   className="shrink-0 text-xs text-blue-600 dark:text-blue-400 opacity-0 group-hover:opacity-100 hover:bg-blue-50 dark:hover:bg-blue-900/30 rounded px-1 transition-opacity"
                                   title="引用此文件"
                                 >+</button>
                                 <span className="font-medium text-gray-700 dark:text-gray-300 truncate max-w-[65%]">{ev.path}</span>
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
                    {session?.id && (
                      <details className="mt-1 text-xs text-gray-500 dark:text-gray-400">
                        <summary className="cursor-pointer select-none hover:text-purple-600 dark:hover:text-purple-300">
                          🧠 {t('ai_reasoning')}
                        </summary>
                        <AiEventTimeline sessionId={session.id} turnNumber={turnNumberFor(i)} />
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
            {progress ? (
              <span className="flex items-center gap-2">
                <span>{progress.message}</span>
                {progress.total > 0 && (
                  <span className="inline-flex items-center gap-1">
                    <span className="text-xs tabular-nums text-gray-400">{progress.current}/{progress.total}</span>
                    <span className="w-20 h-1.5 rounded-full bg-gray-200 dark:bg-gray-700 overflow-hidden">
                      <span
                        className="block h-full rounded-full bg-blue-400 transition-all duration-300"
                        style={{ width: `${progress.total > 0 ? Math.min(100, (progress.current / progress.total) * 100) : 0}%` }}
                      />
                    </span>
                  </span>
                )}
              </span>
            ) : (
              <span>{t('thinking')} {pendingStartedAt ? <ElapsedTimer startedAt={pendingStartedAt} /> : null}</span>
            )}
            <button
              onClick={handleCancel}
              className="ml-1 px-1.5 py-0.5 text-xs text-red-500 hover:text-red-600 hover:bg-red-50 dark:hover:bg-red-900/30 rounded transition-colors"
            >
              {t('cancel')}
            </button>
          </div>
        )}
        {streaming && streaming.sessionId === session?.id && (
          <div className="space-y-2">
            {streaming.reasoning && (
              <details open className="group">
                <summary className="text-xs text-gray-400 dark:text-gray-500 cursor-pointer select-none hover:text-gray-600 dark:hover:text-gray-300 transition-colors">
                  💭 思考中 ({streaming.reasoning.length} chars)...
                </summary>
                <div className="mt-1 text-xs text-gray-400 dark:text-gray-500 whitespace-pre-wrap border-l border-gray-200 dark:border-gray-700 pl-3 max-h-48 overflow-y-auto">
                  {streaming.reasoning}
                </div>
              </details>
            )}
            {streaming.text && (
              <div className="text-sm text-gray-600 dark:text-gray-300 whitespace-pre-wrap border-l-2 border-gray-200 dark:border-gray-700 pl-3">
                {streaming.text}
              </div>
            )}
          </div>
        )}
      </div>

      {conditionChips.length > 0 && (
        <div className="px-4 py-1.5 border-t border-gray-200 dark:border-gray-800 flex flex-wrap gap-1">
          {/* /命令条件 chips：/ext /date /模糊 */}
          {conditionChips.map((c, i) => (
            <span
              key={`${c.kind}-${i}`}
              className="inline-flex items-center gap-1 px-2 py-0.5 text-[10px] rounded-full bg-blue-50 dark:bg-blue-900/20 text-blue-700 dark:text-blue-300 border border-blue-200 dark:border-blue-800"
              title={c.kind === 'fuzzy' ? t('fuzzy_hint') : undefined}
            >
              <span className="font-mono">/{c.kind}:{c.value}</span>
            </span>
          ))}
        </div>
      )}

      {/* /范围: 打字中预览：发送前即时反映"将切换检索范围" */}
      {scopeActionPreview != null && (
        <div className="px-4 py-1 border-t border-gray-200 dark:border-gray-800 flex items-center gap-2 text-[10px]">
          <span className="text-gray-500 dark:text-gray-400">{t('scope_preview')}:</span>
          <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full bg-purple-50 dark:bg-purple-900/20 text-purple-700 dark:text-purple-300 border border-purple-200 dark:border-purple-800">
            {scopeActionPreview === 'clear'
              ? t('scope_preview_all')
              : `${t('scope_preview_dir')}: ${scopeActionPreview.slice(4)}`}
          </span>
        </div>
      )}

      {llmEnabled && (
        <div className="px-4 py-1 border-t border-gray-200 dark:border-gray-800 flex flex-wrap items-center gap-x-3 gap-y-1 text-[10px]">
          <span className="text-gray-400 dark:text-gray-500">{t('scope_range')}:</span>
          {/* 会话级统一范围（合并后）：空串=全库，目录/文件逐条可删 */}
          {mergeScopePrefixes(session?.retrieval_scope ?? []).map((p, i) => (
            <span key={`${p}-${i}`} className="flex items-center gap-1 px-2 py-0.5 rounded-full bg-blue-50 dark:bg-blue-900/20 text-blue-700 dark:text-blue-300 border border-blue-200 dark:border-blue-800 max-w-[220px]">
              {p === '' ? (
                <span className="font-medium">{t('scope_all')}</span>
              ) : (
                <>{/\.\w{1,6}$/.test(p) ? '📄' : '📁'} <span className="truncate">{p}</span></>
              )}
              <button type="button" title={t('clear_scope')} onClick={() => {
                if (!session) return
                if (p === '') {
                  // 空串=全库 → 清除整个范围
                  onSessionChange({ ...session, retrieval_scope: [] })
                } else {
                  // 删掉原始数组中对应路径
                  const orig = session.retrieval_scope ?? []
                  const idx = orig.lastIndexOf(p)
                  if (idx !== -1) {
                    onSessionChange({ ...session, retrieval_scope: orig.filter((_, j) => j !== idx) })
                  }
                }
              }} className="hover:text-blue-600 shrink-0 leading-none">×</button>
            </span>
          ))}
          {/* 空态：范围未生效 → 全库检索 */}
          {session && (session.retrieval_scope?.length ?? 0) === 0 && (
            <span className="text-gray-400 dark:text-gray-500">{t('no_scope')}</span>
          )}
          {/* 严格模式 toggle（仅依据文档） */}
          <button
            type="button"
            onClick={() => session && onSessionChange({ ...session, strict_docs: !session.strict_docs })}
            className={`inline-flex items-center gap-1 px-2 py-0.5 rounded transition-colors ${
              session?.strict_docs
                ? 'bg-green-100 dark:bg-green-900/30 text-green-700 dark:text-green-300 border border-green-300 dark:border-green-700'
                : 'text-gray-500 dark:text-gray-400 border border-gray-200 dark:border-gray-700 hover:bg-gray-50 dark:hover:bg-gray-800'
            }`}
          >
            <span className={`size-1.5 rounded-full ${session?.strict_docs ? 'bg-green-500' : 'bg-gray-300 dark:bg-gray-600'}`} />
            {t('strict_docs')}
          </button>
          {/* 全量召回 toggle（检索与注入不截断） */}
          <button
            type="button"
            onClick={() => session && onSessionChange({ ...session, full_recall: !session.full_recall })}
            className={`inline-flex items-center gap-1 px-2 py-0.5 rounded transition-colors ${
              session?.full_recall
                ? 'bg-purple-100 dark:bg-purple-900/30 text-purple-700 dark:text-purple-300 border border-purple-300 dark:border-purple-700'
                : 'text-gray-500 dark:text-gray-400 border border-gray-200 dark:border-gray-700 hover:bg-gray-50 dark:hover:bg-gray-800'
            }`}
          >
            <span className={`size-1.5 rounded-full ${session?.full_recall ? 'bg-purple-500' : 'bg-gray-300 dark:bg-gray-600'}`} />
            {t('full_recall')}
          </button>
        </div>
      )}

      {/* 合并生效范围摘要：发送前实时显示"本轮将搜哪些" */}
      {effectiveScope.length > 0 && (
        <div className="px-4 py-1 border-t border-gray-200 dark:border-gray-800 flex flex-wrap items-center gap-1.5 text-[10px]">
          <span className="text-gray-400 dark:text-gray-500">{t('effective_scope')}:</span>
          {effectiveScope.map((s, i) => (
            <span key={i} className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 text-gray-600 dark:text-gray-300">
              {s}
            </span>
          ))}
        </div>
      )}

      {llmEnabled ? (
        <div className="relative px-4 py-2 border-t border-gray-200 dark:border-gray-800 flex gap-2 items-start">
          <MentionPicker
            query={mentionQuery}
            position={mentionPos}
            onSelect={handleMentionSelect}
            onClose={() => { setMentionQuery(''); setMentionPos(null) }}
          />
          <div
            className="flex-1 flex flex-wrap items-center gap-1 px-3 py-1.5 text-xs border border-gray-200 dark:border-gray-700 rounded bg-gray-50 dark:bg-gray-800 min-h-[32px] focus-within:ring-1 focus-within:ring-purple-500"
            onDragOver={e => e.preventDefault()}
            onDrop={e => { e.preventDefault(); const p = e.dataTransfer.getData('text/plain'); if (p) insertMention(p) }}
          >
            {mentionChips.map((chip, i) => (
              <span
                key={`${chip.path}-${i}`}
                className="inline-flex items-center gap-1 px-2 py-0.5 text-[10px] rounded-full bg-purple-50 dark:bg-purple-900/20 text-purple-700 dark:text-purple-300 border border-purple-200 dark:border-purple-800 max-w-[180px] shrink-0"
              >
                <span>{chip.isFile ? '📄' : '📁'}</span>
                <span className="truncate">{chip.path}</span>
                <button
                  type="button"
                  onClick={() => handleChipRemove(chip.path)}
                  className="ml-0.5 text-purple-400 hover:text-purple-600 dark:hover:text-purple-200 shrink-0 leading-none"
                >
                  ×
                </button>
              </span>
            ))}
            <input
              ref={inputRef}
              type="text"
              value={input}
              onChange={e => handleInputChange(e.target.value)}
              onKeyDown={e => {
                if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleSend() }
                if (e.key === 'Backspace' && !input && mentionChips.length > 0) {
                  handleChipRemove(mentionChips[mentionChips.length - 1]!.path)
                }
              }}
              onDragOver={e => e.preventDefault()}
              onDrop={e => { e.preventDefault(); const p = e.dataTransfer.getData('text/plain'); if (p) insertMention(p) }}
              placeholder={mentionChips.length > 0 ? '' : (sourceIds.length > 0 ? t('ask_followup') : t('ask_question'))}
              disabled={loading}
              className="flex-1 min-w-[60px] border-none bg-transparent text-gray-700 dark:text-gray-300 placeholder-gray-400 focus:outline-none p-0 disabled:opacity-40"
            />
          </div>
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


