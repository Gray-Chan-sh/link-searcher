import { useEffect, useState } from 'react'
import { aiCapabilities, type AiCapabilities } from '../api/files'
import { useI18n } from '../i18n'
import ChatPanel from '../components/ChatPanel'

export default function AiChat() {
  const { t } = useI18n()
  const [aiCap, setAiCap] = useState<AiCapabilities>({ embedding: false, llm: false })

  useEffect(() => { aiCapabilities().then(setAiCap).catch(() => {}) }, [])

  return (
    <div className="h-full flex flex-col p-6 overflow-y-auto">
      <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100 mb-1">{t('ai_chat')}</h2>
      <p className="text-sm text-gray-500 dark:text-gray-400 mb-4">{t('ai_chat_desc')}</p>
      <div className="flex-1 flex flex-col min-h-0 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg overflow-hidden">
        {aiCap.llm ? (
          <ChatPanel llmEnabled />
        ) : (
          <div className="flex-1 flex items-center justify-center text-sm text-gray-400 px-6 text-center">
            {t('ai_llm_unavailable')}
          </div>
        )}
      </div>
    </div>
  )
}