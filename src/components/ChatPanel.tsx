import { useState, useRef, useEffect, useCallback } from 'react'
import { useI18n } from '../i18n'
import { LoadingSpinner } from '../icons'
import { smartSearch, conversationAsk, type ChatMessage, type SmartSearchResponse } from '../api/files'

interface ChatPanelProps {
  llmEnabled: boolean
}

export default function ChatPanel({ llmEnabled }: ChatPanelProps) {
  const { t } = useI18n()
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [input, setInput] = useState('')
  const [loading, setLoading] = useState(false)
  const [sourceIds, setSourceIds] = useState<string[]>([])
  const [sourceFiles, setSourceFiles] = useState<string[]>([])
  const scrollRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: 'smooth' })
  }, [messages])

  const handleSend = useCallback(async () => {
    const q = input.trim()
    if (!q || loading) return
    setInput('')
    const userMsg: ChatMessage = { role: 'user', content: q }
    const updated = [...messages, userMsg]
    setMessages(updated)
    setLoading(true)

    try {
      if (sourceIds.length === 0) {
        // First message — do smart_search
        const res: SmartSearchResponse = await smartSearch(q)
        setSourceIds(res.source_ids)
        setSourceFiles(res.source_files)
        const assistantMsg: ChatMessage = { role: 'assistant', content: res.answer }
        setMessages([...updated, assistantMsg])
      } else {
        // Follow-up — conversation_ask with existing source docs
        const answer = await conversationAsk([...updated], sourceIds)
        const assistantMsg: ChatMessage = { role: 'assistant', content: answer }
        setMessages([...updated, assistantMsg])
      }
    } catch (e) {
      const err = e instanceof Error ? e.message : '请求失败'
      setMessages([...updated, { role: 'assistant', content: `❌ ${err}` }])
    } finally {
      setLoading(false)
    }
  }, [input, loading, messages, sourceIds])

  const handleNewSession = useCallback(() => {
    setMessages([])
    setSourceIds([])
    setSourceFiles([])
    setInput('')
  }, [])

  if (!llmEnabled) {
    return (
      <div className="flex-1 flex items-center justify-center text-sm text-gray-400">
        {t('ai_llm_unavailable')}
      </div>
    )
  }

  return (
    <div className="flex-1 flex flex-col min-h-0">
      {/* Source files indicator */}
      {sourceFiles.length > 0 && (
        <div className="px-4 py-2 text-xs text-gray-500 dark:text-gray-400 bg-purple-50 dark:bg-purple-900/10 border-b border-purple-100 dark:border-purple-800/30 flex items-center gap-2">
          <span className="text-purple-600 dark:text-purple-400">✦</span>
          <span>{t('source_files', { n: sourceFiles.length })}</span>
          <button
            onClick={handleNewSession}
            className="ml-auto text-purple-600 dark:text-purple-400 hover:underline"
          >
            {t('new_session')}
          </button>
        </div>
      )}

      {/* Messages */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto px-4 py-3 space-y-3">
        {messages.length === 0 && (
          <div className="text-center text-sm text-gray-400 py-12">
            {t('chat_placeholder')}
          </div>
        )}
        {messages.map((m, i) => (
          <div key={i} className={`flex ${m.role === 'user' ? 'justify-end' : 'justify-start'}`}>
            <div className={`max-w-[85%] px-3 py-2 rounded-lg text-sm whitespace-pre-wrap ${
              m.role === 'user'
                ? 'bg-blue-600 text-white'
                : 'bg-gray-100 dark:bg-gray-800 text-gray-800 dark:text-gray-200'
            }`}>
              {m.content}
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

      {/* Input */}
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
          disabled={loading || !input.trim()}
          className="px-3 py-1.5 text-xs font-medium text-white bg-purple-600 hover:bg-purple-700 rounded disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
        >
          {t('send')}
        </button>
      </div>
    </div>
  )
}