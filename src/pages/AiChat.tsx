import { useEffect, useState, useCallback } from 'react'
import { aiCapabilities, listChatSessions, createChatSession, deleteChatSession, loadChatSession as loadChatSessionById, saveChatSession, exportChatSession, type AiCapabilities, type ChatSession, type ChatSessionMeta } from '../api/files'
import { useI18n } from '../i18n'
import { writeTextFile } from '@tauri-apps/plugin-fs'
import { save, ask } from '@tauri-apps/plugin-dialog'
import { PlusIcon, TrashIcon } from '../icons'
import ChatPanel from '../components/ChatPanel'

export default function AiChat() {
  const { t } = useI18n()
  const [aiCap, setAiCap] = useState<AiCapabilities>({ embedding: false, llm: false })
  const [capFailed, setCapFailed] = useState(false)
  const [sessions, setSessions] = useState<ChatSessionMeta[]>([])
  const [activeId, setActiveId] = useState<string | null>(null)
  const [activeSession, setActiveSession] = useState<ChatSession | null>(null)

  const refreshCapabilities = useCallback(() => {
    setCapFailed(false)
    aiCapabilities().then(setAiCap).catch(() => setCapFailed(true))
  }, [])

  useEffect(() => { refreshCapabilities() }, [refreshCapabilities])

  const refreshList = useCallback(async () => {
    try { setSessions(await listChatSessions()) } catch { /* ignore */ }
  }, [])

  useEffect(() => { refreshList() }, [refreshList])

  // Ensure a session exists when chat is enabled (create one if none).
  useEffect(() => {
    if (!aiCap.llm || activeId) return
    if (sessions.length === 0) {
      createChatSession().then(id => {
        setActiveId(id)
        setActiveSession({ id, title: '', created_at: 0, updated_at: 0, messages: [], source_ids: [], source_files: [] })
        refreshList()
      }).catch(() => {})
    } else {
      const latest = sessions[0].id
      setActiveId(latest)
      loadSession(latest)
    }
  }, [aiCap.llm, sessions.length, activeId])

  const loadSession = useCallback(async (id: string) => {
    setActiveId(id)
    try {
      const s = await loadChatSessionById(id)
      if (s) {
        setActiveSession(s)
      } else {
        // Session vanished (e.g. stale id from a not-yet-persisted file):
        // release activeId so the ensure-session effect re-runs and self-heals.
        setActiveId(null)
        refreshList()
      }
    } catch {
      setActiveId(null)
    }
  }, [refreshList])

  const handleNewSession = useCallback(async () => {
    try {
      const id = await createChatSession()
      setActiveSession({ id, title: '', created_at: 0, updated_at: 0, messages: [], source_ids: [], source_files: [] })
      setActiveId(id)
      refreshList()
    } catch { /* ignore */ }
  }, [refreshList])

  const handleDelete = useCallback(async (id: string) => {
    const confirmed = await ask(t('confirm_delete_session'), { title: t('delete'), kind: 'warning' })
    if (!confirmed) return
    try {
      await deleteChatSession(id)
      if (id === activeId) {
        setActiveSession(null)
        setActiveId(null)
        refreshList()
      } else {
        refreshList()
      }
    } catch { /* ignore */ }
  }, [activeId, refreshList])

  const handleSessionChange = useCallback((session: ChatSession | null) => {
    setActiveSession(session)
    if (session) {
      saveChatSession(session).then(refreshList).catch(() => {})
    }
  }, [refreshList])

  const handleExport = useCallback(async () => {
    if (!activeId) return
    try {
      const md = await exportChatSession(activeId)
      const path = await save({ defaultPath: 'ai-chat.md', filters: [{ name: 'Markdown', extensions: ['md'] }] })
      if (path) await writeTextFile(path, md)
    } catch (e) {
      // 不再静默吞错 — 保存失败（如路径无写权限）必须让用户可见。
      alert(`导出失败: ${e instanceof Error ? e.message : String(e)}`)
    }
  }, [activeId])

  return (
    <div className="h-full flex p-3 overflow-hidden">
      {/* Session sidebar */}
      <div className="w-56 shrink-0 flex flex-col bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg mr-3 overflow-hidden">
        <div className="px-3 py-2 flex items-center justify-between border-b border-gray-200 dark:border-gray-800">
          <span className="text-xs font-medium text-gray-500 dark:text-gray-400">{t('sessions')}</span>
          <button
            onClick={handleNewSession}
            className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500 dark:text-gray-400"
            title={t('new_session')}
          >
            <PlusIcon className="size-3.5" />
          </button>
        </div>
        <div className="flex-1 overflow-y-auto p-1 space-y-0.5">
          {sessions.map(s => (
            <div
              key={s.id}
              onClick={() => loadSession(s.id)}
              className={`group flex items-center gap-1 px-2 py-1.5 rounded text-xs cursor-pointer transition-colors ${
                s.id === activeId
                  ? 'bg-purple-50 dark:bg-purple-900/20 text-purple-700 dark:text-purple-300'
                  : 'text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800'
              }`}
            >
              <span className="flex-1 truncate">{s.title}</span>
              <button
                onClick={e => { e.stopPropagation(); handleDelete(s.id) }}
                className="opacity-0 group-hover:opacity-100 p-0.5 rounded hover:bg-red-100 dark:hover:bg-red-900/30 text-gray-400 hover:text-red-500 transition-opacity"
                title={t('delete')}
              >
                <TrashIcon className="size-3" />
              </button>
            </div>
          ))}
        </div>
      </div>

      {/* Chat area */}
      <div className="flex-1 flex flex-col min-w-0 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg overflow-hidden">
        <div className="px-4 py-2 border-b border-gray-200 dark:border-gray-800 flex items-center justify-between">
          <span className="text-sm font-medium text-gray-900 dark:text-gray-100">
            {activeSession?.title || t('ai_chat')}
          </span>
          <div className="flex items-center gap-2">
            <button
              onClick={handleExport}
              disabled={!activeId}
              className="px-2 py-1 text-xs font-medium text-gray-500 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors disabled:opacity-40"
            >
              {t('export')}
            </button>
          </div>
        </div>
        {activeSession ? (
          <ChatPanel llmEnabled={aiCap.llm} session={activeSession} onSessionChange={handleSessionChange} />
        ) : capFailed ? (
          <div className="flex-1 flex flex-col items-center justify-center gap-3 text-sm text-gray-400">
            <span>{t('ai_llm_unavailable')}</span>
            <button
              onClick={refreshCapabilities}
              className="px-3 py-1.5 text-xs font-medium text-white bg-purple-600 hover:bg-purple-700 rounded transition-colors"
            >
              {t('retry')}
            </button>
          </div>
        ) : (
          <div className="flex-1 flex items-center justify-center text-sm text-gray-400">
            {t('ai_llm_unavailable')}
          </div>
        )}
      </div>
    </div>
  )
}