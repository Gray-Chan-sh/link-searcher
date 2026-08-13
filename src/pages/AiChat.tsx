import { useEffect, useState, useCallback } from 'react'
import { aiCapabilities, listChatSessions, createChatSession, deleteChatSession, loadChatSession as loadChatSessionById, saveChatSession, exportChatSession, searchFilePaths, type AiCapabilities, type ChatSession, type ChatSessionMeta } from '../api/files'
import { listDirs, getDirChildren, type DirTreeNode } from '../api/dirs'
import { useI18n } from '../i18n'
import { writeTextFile } from '@tauri-apps/plugin-fs'
import { save, ask } from '@tauri-apps/plugin-dialog'
import { PlusIcon, TrashIcon, FolderIcon } from '../icons'
import ChatPanel from '../components/ChatPanel'

export default function AiChat() {
  const { t } = useI18n()
  const [aiCap, setAiCap] = useState<AiCapabilities>({ embedding: false, llm: false })
  const [capFailed, setCapFailed] = useState(false)
  const [sessions, setSessions] = useState<ChatSessionMeta[]>([])
  const [activeId, setActiveId] = useState<string | null>(null)
  const [activeSession, setActiveSession] = useState<ChatSession | null>(null)
  // 树状文件浏览器
  const [dirTrees, setDirTrees] = useState<{ id: string; basePath: string; label: string; root: DirTreeNode | null }[]>([])
  const [treeExpanded, setTreeExpanded] = useState(false)
  const [pendingMention, setPendingMention] = useState<string | null>(null)

  const refreshCapabilities = useCallback(() => {
    setCapFailed(false)
    aiCapabilities().then(setAiCap).catch(() => setCapFailed(true))
  }, [])

  useEffect(() => { refreshCapabilities() }, [refreshCapabilities])

  const refreshList = useCallback(async () => {
    try { setSessions(await listChatSessions()) } catch { /* ignore */ }
  }, [])

  useEffect(() => { refreshList() }, [refreshList])

  // 加载目录树（懒加载：只加载根层，展开时按需加载子目录）
  useEffect(() => {
    listDirs().then(dirs => {
      setDirTrees(dirs.map(d => {
        const label = d.alias || d.path.split('/').pop() || d.path
        return { id: d.id, basePath: d.path, label, root: null }
      }))
      dirs.forEach(d => {
        getDirChildren(d.path).then(items => {
          setDirTrees(prev => prev.map(x => x.id === d.id ? { ...x, root: items } : x))
        }).catch(() => {})
      })
    }).catch(() => {})
  }, [])

  const handleTreeClick = useCallback((path: string) => {
    setPendingMention(path)
  }, [])

  // 专注模式：会话仅分析此文件（临时屏蔽其他范围），追问持续直到退出
  const handleFocusFile = useCallback((path: string) => {
    if (activeSession) handleSessionChange({ ...activeSession, focus_file: path })
  }, [activeSession, handleSessionChange])

  // /范围:全库或目录路径 → 解析为 dir_id 并更新会话范围
  const handleScopeAction = useCallback((action: string) => {
    if (!activeSession) return
    if (action === 'clear') {
      handleSessionChange({ ...activeSession, scope_dir_ids: [] })
      return
    }
    if (action.startsWith('dir:')) {
      const dirName = action.slice(4)
      // 匹配 dirTrees 中 label 或 basePath 尾部命中的目录
      const hit = dirTrees.find(dt => dt.label === dirName || dt.basePath.endsWith('/' + dirName))
      if (hit) {
        handleSessionChange({ ...activeSession, scope_dir_ids: [hit.id] })
      }
    }
  }, [activeSession, dirTrees, handleSessionChange])

  // 树状根目录的会话范围设置（对应的 dirTree id）
  const handleSetSessionScope = useCallback((dirId: string) => {
    if (activeSession) handleSessionChange({ ...activeSession, scope_dir_ids: [dirId] })
  }, [activeSession, handleSessionChange])

  const handleClearSessionScope = useCallback(() => {
    if (activeSession) handleSessionChange({ ...activeSession, scope_dir_ids: [] })
  }, [activeSession, handleSessionChange])

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
        {/* 文件树面板 */}
        <div className="border-t border-gray-200 dark:border-gray-800">
          <button
            onClick={() => setTreeExpanded(v => !v)}
            className="w-full px-3 py-2 flex items-center justify-between text-xs font-medium text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300 transition-colors"
          >
            <span className="flex items-center gap-1"><FolderIcon className="size-3" /> {t('file_tree')}</span>
            <span className="text-[10px]">{treeExpanded ? '▾' : '▸'}</span>
          </button>
          {treeExpanded && (
            <div className="max-h-48 overflow-y-auto p-1 space-y-1">
              {dirTrees.map(dt => (
                <div key={dt.id}>
                  <div className="px-2 py-0.5 flex items-center gap-1">
                    <span className="flex-1 text-[10px] font-medium text-gray-400 dark:text-gray-500 truncate">{dt.label}</span>
                    <button
                      type="button"
                      onClick={() => activeSession?.scope_dir_ids?.includes(dt.id) ? handleClearSessionScope() : handleSetSessionScope(dt.id)}
                      title={activeSession?.scope_dir_ids?.includes(dt.id) ? t('clear_session_scope') : t('set_session_scope')}
                      className={`text-[10px] ${
                        activeSession?.scope_dir_ids?.includes(dt.id)
                          ? 'text-purple-600 dark:text-purple-300 font-medium'
                          : 'text-gray-400 hover:text-purple-500 dark:text-gray-500 dark:hover:text-purple-400'
                      } shrink-0`}
                    >
                      {activeSession?.scope_dir_ids?.includes(dt.id) ? '范围✓' : '范围'}
                    </button>
                  </div>
                  {dt.root && dt.root.map(child => (
                    <TreeFileList key={child.path} node={child} basePath={dt.basePath} onPick={handleTreeClick} onFocus={handleFocusFile} />
                  ))}
                </div>
              ))}
              {dirTrees.length === 0 && (
                <div className="px-2 py-1 text-[10px] text-gray-400">{t('no_dirs')}</div>
              )}
            </div>
          )}
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
          <ChatPanel
            llmEnabled={aiCap.llm}
            session={activeSession}
            onSessionChange={handleSessionChange}
            pendingMention={pendingMention}
            onMentionConsumed={() => setPendingMention(null)}
            onScopeAction={handleScopeAction}
          />
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

/** 递归文件树：目录左键展开/折叠、右键「加入对话」；文件左键「加入对话」、右键文件「专注分析」。
 *  `node.path` 为绝对路径，`basePath` 为目录根，点击时转相对路径（与 file_tracking 一致）。 */
function TreeFileList({ node, basePath, onPick, onFocus }: {
  node: DirTreeNode
  basePath: string
  onPick: (relPath: string) => void
  onFocus?: (relPath: string) => void
}) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [children, setChildren] = useState<DirTreeNode[] | null>(
    node.children.length > 0 ? node.children : null,
  )
  const [loading, setLoading] = useState(false)
  const isDir = node.is_dir || node.children.length > 0
  const rel = node.path.startsWith(basePath)
    ? node.path.slice(basePath.length).replace(/^\/+/, '')
    : node.path

  const handleToggle = useCallback(() => {
    if (!isDir) return
    if (open) { setOpen(false); return }
    // 首次展开：如果子项未加载，调 API 懒加载
    if (children === null && !loading) {
      setLoading(true)
      getDirChildren(node.path).then(items => {
        setChildren(items)
        setOpen(true)
        setLoading(false)
      }).catch(() => setLoading(false))
    } else {
      setOpen(true)
    }
  }, [isDir, open, children, loading, node.path])

  return (
    <div>
<button
        type="button"
        draggable
        onDragStart={e => { e.dataTransfer.setData('text/plain', rel); e.dataTransfer.effectAllowed = 'copy' }}
        onClick={handleToggle}
        onContextMenu={e => { e.preventDefault(); if (!isDir && onFocus) { onFocus(rel) } else { onPick(rel) } }}
        className="w-full flex items-center gap-1 px-2 py-0.5 text-xs text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800 rounded transition-colors"
        title={isDir ? `${t('file_tree_dir_hint')}${rel}` : `${t('file_tree_file_hint')}${rel}`}
      >
        <span className="text-[10px] shrink-0">{isDir ? (open ? '▾' : (loading ? '⋯' : '▸')) : '📄'}</span>
        <span className="truncate">{node.name}</span>
        {!isDir && <span className="ml-auto text-[9px] text-gray-400 opacity-0 group-hover:opacity-100">+</span>}
      </button>
      {open && children && children.map(child => (
        <div key={child.path} className="pl-3">
          <TreeFileList key={child.path} node={child} basePath={basePath} onPick={onPick} />
        </div>
      ))}
    </div>
  )
}