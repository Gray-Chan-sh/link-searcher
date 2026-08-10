import { useState, useRef, useEffect, useCallback } from 'react'
import ReactMarkdown from 'react-markdown'
import { useI18n } from '../i18n'
import { LoadingSpinner } from '../icons'
import { smartSearch, conversationAsk, openFile, type ChatMessage, type SmartSearchResponse, type ChatSession } from '../api/files'

interface ChatPanelProps {
  llmEnabled: boolean
  session: ChatSession | null
  onSessionChange: (session: ChatSession | null) => void
}

export default function ChatPanel({ llmEnabled, session, onSessionChange }: ChatPanelProps) {
  const { t } = useI18n()
  const [input, setInput] = useState('')
  const [loading, setLoading] = useState(false)
  const [showSources, setShowSources] = useState(false)
  const scrollRef = useRef<HTMLDivElement>(null)

  const messages = session?.messages ?? []
  const sourceIds = session?.source_ids ?? []
  const sourceFiles = session?.source_files ?? []

  const patchSession = useCallback((patch: Partial<ChatSession>) => {
    if (session) onSessionChange({ ...session, ...patch })
  }, [session, onSessionChange])

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: 'smooth' })
  }, [messages])

  const handleSend = useCallback(async () => {
    const q = input.trim()
    if (!q || loading || !session) return
    setInput('')
    const userMsg: ChatMessage = { role: 'user', content: q }
    patchSession({ messages: [...messages, userMsg] })

    setLoading(true)
    try {
      if (sourceIds.length === 0) {
        const res: SmartSearchResponse = await smartSearch(q)
        patchSession({
          messages: [...messages, userMsg, { role: 'assistant', content: res.answer }],
          source_ids: res.source_ids,
          source_files: res.source_files,
        })
      } else {
        const answer = await conversationAsk([...messages, userMsg], sourceIds)
        patchSession({ messages: [...messages, userMsg, { role: 'assistant', content: answer }] })
      }
    } catch (e) {
      // Surface the backend's real error string — Tauri's reject value may
      // be an Error, a string, or an object, so be tolerant instead of
      // collapsing everything to a generic "请求失败".
      const err =
        e instanceof Error && e.message
          ? e.message
          : typeof e === 'string'
            ? e
            : typeof e === 'object' && e !== null && 'message' in e
              ? String((e as { message: unknown }).message)
              : String(e)
      patchSession({ messages: [...messages, userMsg, { role: 'assistant', content: `❌ ${err}` }] })
    } finally {
      setLoading(false)
    }
  }, [input, loading, session, messages, sourceIds, patchSession])

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
                : <ReactMarkdown>{m.content}</ReactMarkdown>
              }
            </div>
          </div>
        ))}
        {loading && (
          <div className="flex items-center gap-2 text-sm text-gray-500">
            <LoadingSpinner className="size-3.5" />
            <span>{t('thinking')}</span>
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
