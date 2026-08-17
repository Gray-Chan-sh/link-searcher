import { useEffect, useState, useCallback, useMemo, useRef } from 'react'
import { aiCapabilities, listChatSessions, createChatSession, deleteChatSession, loadChatSession as loadChatSessionById, saveChatSession, exportChatSession, type AiCapabilities, type ChatSession, type ChatSessionMeta } from '../api/files'
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
  const [dirTrees, setDirTrees] = useState<{ id: string; basePath: string; label: string; root: DirTreeNode[] | null; private: boolean }[]>([])
  const [treeExpanded, setTreeExpanded] = useState(false)
  const [pendingMention, setPendingMention] = useState<string | null>(null)
  // 会话列表搜索与时间筛选
  const [sessionFilter, setSessionFilter] = useState('')
  const [sessionRange, setSessionRange] = useState<'all' | 'today' | 'week' | 'older'>('all')
  // 侧栏宽度（默认 224px，可拖拽，localStorage 持久化）
  const [sidebarW, setSidebarW] = useState(() => {
    const saved = Number(localStorage.getItem('ls_chat_sidebar_w'))
    return saved >= 160 && saved <= 480 ? saved : 224
  })

  // 会话最后活跃时间的本地化显示：今天 → HH:mm，否则 → 日期
  const fmtSessionTime = useCallback((ts: number) => {
    if (!ts) return ''
    const d = new Date(ts * 1000)
    const now = new Date()
    const sameDay = d.toDateString() === now.toDateString()
    if (sameDay) {
      return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    }
    return d.toLocaleDateString([], { month: '2-digit', day: '2-digit' })
  }, [])

  // 时间范围过滤：今日 / 本周 / 更早
  const rangeFilter = useCallback((updatedAt: number): boolean => {
    if (sessionRange === 'all') return true
    const d = new Date(updatedAt * 1000)
    const now = new Date()
    const sameDay = d.toDateString() === now.toDateString()
    if (sessionRange === 'today') return sameDay
    if (sessionRange === 'week') {
      const weekAgo = now.getTime() - 7 * 24 * 60 * 60 * 1000
      return d.getTime() >= weekAgo && !sameDay // week = 本周非今日，older = 更早
    }
    const weekAgo = now.getTime() - 7 * 24 * 60 * 60 * 1000
    return d.getTime() < weekAgo
  }, [sessionRange])

  const visibleSessions = useMemo(() => {
    const q = sessionFilter.trim().toLowerCase()
    return sessions.filter(s =>
      (!q || s.title.toLowerCase().includes(q)) && rangeFilter(s.updated_at)
    )
  }, [sessions, sessionFilter, rangeFilter])

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
        return { id: d.id, basePath: d.path, label, root: null, private: d.private ?? false }
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

  const handleSessionChange = useCallback((session: ChatSession | null) => {
    setActiveSession(session)
    if (session) {
      saveChatSession(session).then(refreshList).catch(() => {})
    }
  }, [refreshList])

  // 统一范围入口：把路径加入会话级检索范围（目录/文件统一；父路径吞并子路径在后端做）
  const handleAddToScope = useCallback((path: string) => {
    if (!activeSession) return
    const cur = activeSession.retrieval_scope ?? []
    if (!cur.includes(path)) {
      handleSessionChange({ ...activeSession, retrieval_scope: [...cur, path] })
    }
  }, [activeSession, handleSessionChange])

  // /范围:全库或目录路径 → 解析为路径并更新会话范围
  const handleScopeAction = useCallback((action: string) => {
    if (!activeSession) return
    if (action === 'clear') {
      handleSessionChange({ ...activeSession, retrieval_scope: [] })
      return
    }
    if (action.startsWith('dir:')) {
      const dirName = action.slice(4)
      // 匹配 dirTrees 中 label 或 basePath 尾部命中的目录
      const hit = dirTrees.find(dt => dt.label === dirName || dt.basePath.endsWith('/' + dirName))
      if (hit) {
        handleAddToScope(hit.basePath)
      }
    }
  }, [activeSession, dirTrees, handleSessionChange, handleAddToScope])

  // 树状根目录的会话范围设置：加入该监控根绝对路径
  const handleSetSessionScope = useCallback((dirId: string) => {
    const dt = dirTrees.find(d => d.id === dirId)
    if (activeSession && dt) handleAddToScope(dt.basePath)
  }, [activeSession, dirTrees, handleAddToScope])

  const handleClearSessionScope = useCallback(() => {
    if (activeSession) handleSessionChange({ ...activeSession, retrieval_scope: [] })
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

  // 批量管理模式
  const [selectMode, setSelectMode] = useState(false)
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set())

  const toggleSelectMode = useCallback(() => {
    setSelectMode(v => { if (v) setSelectedIds(new Set()); return !v })
  }, [])

  const toggleSelect = useCallback((id: string) => {
    setSelectedIds(prev => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id); else next.add(id)
      return next
    })
  }, [])

  const selectAllVisible = useCallback(() => {
    setSelectedIds(new Set(visibleSessions.map(s => s.id)))
  }, [visibleSessions])

  // 批量删除：确认后逐项删除，最后退出批量模式
  const handleBatchDelete = useCallback(async () => {
    if (selectedIds.size === 0) return
    const confirmed = await ask(
      t('confirm_delete_sessions', { n: selectedIds.size }),
      { title: t('delete'), kind: 'warning' }
    )
    if (!confirmed) return
    const ids = [...selectedIds]
    for (const id of ids) {
      try { await deleteChatSession(id) } catch { /* ignore */ }
    }
    if (ids.includes(activeId ?? '')) {
      setActiveSession(null)
      setActiveId(null)
    }
    setSelectedIds(new Set())
    setSelectMode(false)
    refreshList()
  }, [selectedIds, activeId, refreshList])

  // 批量导出：逐会话导出并合并为一个 Markdown 文件
  const handleBatchExport = useCallback(async () => {
    if (selectedIds.size === 0) return
    try {
      const parts: string[] = []
      for (const id of selectedIds) {
        try { parts.push(await exportChatSession(id)) } catch { /* ignore 单个失败跳过 */ }
      }
      if (parts.length === 0) { alert(t('export_failed', { error: t('no_sessions_match') })); return }
      const combined = parts.join('\n\n---\n\n')
      const path = await save({ defaultPath: 'ai-chats-batch.md', filters: [{ name: 'Markdown', extensions: ['md'] }] })
      if (path) await writeTextFile(path, combined)
    } catch (e) {
      alert(t('export_failed', { error: e instanceof Error ? e.message : String(e) }))
    }
  }, [selectedIds])

  // 侧栏宽度持久化
  useEffect(() => {
    localStorage.setItem('ls_chat_sidebar_w', String(sidebarW))
  }, [sidebarW])

  // 拖拽侧栏右缘调整宽度
  const sidebarResizeRef = useRef<HTMLDivElement>(null)
  const startResize = useCallback((e: React.MouseEvent) => {
    e.preventDefault()
    const startX = e.clientX
    const startW = sidebarW
    const move = (ev: MouseEvent) => {
      const w = Math.min(480, Math.max(160, startW + (ev.clientX - startX)))
      setSidebarW(w)
    }
    const up = () => {
      document.removeEventListener('mousemove', move)
      document.removeEventListener('mouseup', up)
    }
    document.addEventListener('mousemove', move)
    document.addEventListener('mouseup', up)
  }, [sidebarW])

  return (
    <div className="h-full flex p-3 overflow-hidden">
      {/* Session sidebar */}
      <div className="shrink-0 flex flex-col bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg mr-3 overflow-hidden relative"
        style={{ width: sidebarW }}
      >
        <div className="px-3 py-2 flex items-center justify-between border-b border-gray-200 dark:border-gray-800">
          <span className="text-xs font-medium text-gray-500 dark:text-gray-400">{t('sessions')}</span>
          <div className="flex items-center gap-1">
            {selectMode ? (
              <button
                onClick={toggleSelectMode}
                className="px-1.5 py-0.5 text-[10px] rounded bg-gray-100 dark:bg-gray-800 text-gray-600 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-700"
                title={t('exit_select_mode')}
              >
                ✓ {t('done')}
              </button>
            ) : (
              <button
                onClick={toggleSelectMode}
                className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500 dark:text-gray-400"
                title={t('batch_manage')}
              >
                ☑
              </button>
            )}
            <button
              onClick={handleNewSession}
              className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500 dark:text-gray-400"
              title={t('new_session')}
            >
              <PlusIcon className="size-3.5" />
            </button>
          </div>
        </div>
        {/* 会话搜索 + 时间筛选 */}
        <div className="px-2 py-1.5 border-b border-gray-200 dark:border-gray-800 space-y-1">
          <input
            type="text"
            value={sessionFilter}
            onChange={e => setSessionFilter(e.target.value)}
            placeholder={t('search_sessions')}
            className="w-full text-xs bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded px-2 py-1 text-gray-700 dark:text-gray-300 placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500"
          />
          <div className="flex gap-1">
            {(['all', 'today', 'week', 'older'] as const).map(r => (
              <button
                key={r}
                onClick={() => setSessionRange(r)}
                className={`px-1.5 py-0.5 text-[10px] rounded transition-colors ${
                  sessionRange === r
                    ? 'bg-blue-500 text-white'
                    : 'text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700'
                }`}
              >
                {t(`session_range_${r}`)}
              </button>
            ))}
          </div>
        </div>
        <div className="flex-1 overflow-y-auto p-1 space-y-0.5">
          {visibleSessions.map(s => (
            <div
              key={s.id}
              onClick={(e) => {
              // Cmd/Ctrl+点击 = 直接多选（免切模式），与浏览页交互一致
              if (e.metaKey || e.ctrlKey) {
                e.preventDefault()
                setSelectMode(true)
                toggleSelect(s.id)
              } else if (selectMode) {
                toggleSelect(s.id)
              } else {
                loadSession(s.id)
              }
            }}
              className={`group flex items-center gap-1 px-2 py-1.5 rounded text-xs cursor-pointer transition-colors ${
                selectMode
                  ? selectedIds.has(s.id)
                    ? 'bg-blue-50 dark:bg-blue-900/20 text-blue-700 dark:text-blue-300'
                    : 'text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800'
                  : s.id === activeId
                    ? 'bg-purple-50 dark:bg-purple-900/20 text-purple-700 dark:text-purple-300'
                    : 'text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800'
              }`}
            >
              {selectMode && (
                <span className={`shrink-0 size-3.5 rounded border flex items-center justify-center text-[9px] ${
                  selectedIds.has(s.id)
                    ? 'bg-blue-500 border-blue-500 text-white'
                    : 'border-gray-300 dark:border-gray-600'
                }`}>
                  {selectedIds.has(s.id) ? '✓' : ''}
                </span>
              )}
              <span className="flex-1 truncate" title={s.title}>{s.title}</span>
              {fmtSessionTime(s.updated_at) && (
                <span className="text-[10px] text-gray-400 dark:text-gray-500 shrink-0">{fmtSessionTime(s.updated_at)}</span>
              )}
              {!selectMode && (
                <button
                  onClick={e => { e.stopPropagation(); handleDelete(s.id) }}
                  className="opacity-0 group-hover:opacity-100 p-0.5 rounded hover:bg-red-100 dark:hover:bg-red-900/30 text-gray-400 hover:text-red-500 transition-opacity"
                  title={t('delete')}
                >
                  <TrashIcon className="size-3" />
                </button>
              )}
            </div>
          ))}
          {visibleSessions.length === 0 && (
            <div className="px-2 py-4 text-center text-xs text-gray-400 dark:text-gray-500">
              {sessions.length === 0 ? t('no_sessions') : t('no_sessions_match')}
            </div>
          )}
        </div>
        {/* 批量操作栏 */}
        {selectMode && (
          <div className="px-2 py-1.5 border-t border-gray-200 dark:border-gray-800 flex items-center gap-1 flex-wrap">
            <span className="text-[10px] text-gray-500 dark:text-gray-400 flex-1">
              {t('selected_count', { n: selectedIds.size })}
            </span>
            <button
              onClick={selectAllVisible}
              className="px-1.5 py-0.5 text-[10px] rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-500 dark:text-gray-400"
            >
              {t('select_all')}
            </button>
            <button
              onClick={handleBatchExport}
              disabled={selectedIds.size === 0}
              className="px-1.5 py-0.5 text-[10px] rounded bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {t('export')}
            </button>
            <button
              onClick={handleBatchDelete}
              disabled={selectedIds.size === 0}
              className="px-1.5 py-0.5 text-[10px] rounded bg-red-500 text-white hover:bg-red-600 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {t('delete')}
            </button>
          </div>
        )}
        {/* 文件树面板 */}
        <div className="border-t border-gray-200 dark:border-gray-800">
          <button
            onClick={() => setTreeExpanded(v => !v)}
            className="w-full px-3 py-2 flex items-center justify-between text-xs font-medium text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300 transition-colors"
          >
            <span className="flex items-center gap-1.5"><FolderIcon className="size-4" /> <span className="text-sm">{t('file_tree')}</span></span>
            <span className="text-[10px]">{treeExpanded ? '▾' : '▸'}</span>
          </button>
          {treeExpanded && (
            <div className="max-h-72 overflow-y-auto p-2 space-y-1">
              {dirTrees.map(dt => (
                <div key={dt.id}>
                  <div className="px-2 py-0.5 flex items-center gap-1">
                    <span className="flex-1 text-[10px] font-medium text-gray-400 dark:text-gray-500 truncate">{dt.label}</span>
                    <button
                      type="button"
                      onClick={() => activeSession?.retrieval_scope?.includes(dt.basePath) ? handleClearSessionScope() : handleSetSessionScope(dt.id)}
                      title={activeSession?.retrieval_scope?.includes(dt.basePath) ? t('clear_session_scope') : t('set_session_scope')}
                      className={`text-[10px] ${
                        activeSession?.retrieval_scope?.includes(dt.basePath)
                          ? 'text-purple-600 dark:text-purple-300 font-medium'
                          : 'text-gray-400 hover:text-purple-500 dark:text-gray-500 dark:hover:text-purple-400'
                      } shrink-0`}
                    >
                      {activeSession?.retrieval_scope?.includes(dt.basePath) ? '范围✓' : '范围'}
                    </button>
                  </div>
                  {dt.root && sortTreeNodes(dt.root).map(child => (
                    <TreeFileList key={child.path} node={child} basePath={dt.basePath} onPick={handleTreeClick} onScope={handleAddToScope} />
                  ))}
                </div>
              ))}
              {dirTrees.length === 0 && (
                <div className="px-2 py-1 text-[10px] text-gray-400">{t('no_dirs')}</div>
              )}
            </div>
          )}
        </div>
        {/* 拖拽把手：调整会话列表宽度 */}
        <div
          ref={sidebarResizeRef}
          onMouseDown={startResize}
          className="absolute right-0 top-0 bottom-0 w-1.5 cursor-col-resize hover:bg-blue-500/50 active:bg-blue-500"
          title={t('resize_sidebar')}
        />
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

/** 目录优先、按名称排序 */
function sortTreeNodes(nodes: DirTreeNode[]): DirTreeNode[] {
  return [...nodes].sort((a, b) => {
    const aDir = a.is_dir || a.children.length > 0
    const bDir = b.is_dir || b.children.length > 0
    if (aDir !== bDir) return aDir ? -1 : 1 // 目录在前
    return a.name.localeCompare(b.name)
  })
}

/** 递归文件树：左键展开/加入对话；右键文件/目录统一「加入检索范围」。
 *  `node.path` 为绝对路径，`basePath` 为目录根，点击时转相对路径（与 file_tracking 一致）。 */
function TreeFileList({ node, basePath, onPick, onScope }: {
  node: DirTreeNode
  basePath: string
  onPick: (relPath: string) => void
  onScope?: (relPath: string) => void
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

  const sortedChildren = children ? sortTreeNodes(children) : null

  const handleToggle = useCallback(() => {
    if (!isDir) return
    if (open) { setOpen(false); return }
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
        onClick={isDir ? handleToggle : () => onPick(rel)}
        onContextMenu={e => {
          e.preventDefault()
          // 右键统一入口：文件/目录都加入检索范围
          if (onScope) onScope(rel)
          else onPick(rel)
        }}
        className="w-full flex items-center gap-1.5 px-2 py-1 text-sm text-gray-700 dark:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-800 rounded transition-colors"
        title={isDir ? `${t('file_tree_dir_hint')}${rel}` : `${t('file_tree_file_hint')}${rel}`}
      >
        <span className="text-sm shrink-0">{isDir ? (open ? '📂' : (loading ? '⋯' : '📁')) : '📄'}</span>
        <span className="truncate text-xs">{node.name}</span>
        {!isDir && <span className="ml-auto text-[10px] text-gray-400 opacity-0 group-hover:opacity-100">+</span>}
      </button>
      {open && sortedChildren && sortedChildren.map(child => (
        <div key={child.path} className="pl-3">
          <TreeFileList key={child.path} node={child} basePath={basePath} onPick={onPick} onScope={onScope} />
        </div>
      ))}
    </div>
  )
}