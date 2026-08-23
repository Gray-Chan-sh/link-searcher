import { useEffect, useState, useCallback, useMemo, useRef } from 'react'
import { aiCapabilities, listChatSessions, createChatSession, deleteChatSession, loadChatSession as loadChatSessionById, saveChatSession, exportChatSession, exportChatSessionJson, type AiCapabilities, type ChatSession, type ChatSessionMeta } from '../api/files'
import { listDirs, getDirChildren, type DirTreeNode } from '../api/dirs'
import { searchTreePrune } from '../api/files'
import { useI18n } from '../i18n'
import { mergeScopePrefixes } from '../utils/scopeMerge'
import { saveFile, confirm } from '../utils/platform'
import { PlusIcon, TrashIcon, FolderIcon, FolderOpenIcon, FileTextIcon, ChevronDownIcon, LoadingSpinner } from '../icons'
import ChatPanel from '../components/ChatPanel'

type ResultNode = { name: string; path: string; isMatch: boolean; children: ResultNode[] }

function buildResultTree(paths: string[]): ResultNode[] {
  const matchSet = new Set(paths)
  const roots: ResultNode[] = []
  const nodeMap = new Map<string, ResultNode>()
  for (const path of paths) {
    let cur = ''
    let siblings = roots
    for (const seg of path.split('/')) {
      cur = cur ? `${cur}/${seg}` : seg
      let node = nodeMap.get(cur)
      if (!node) {
        node = { name: seg, path: cur, isMatch: matchSet.has(cur), children: [] }
        nodeMap.set(cur, node)
        siblings.push(node)
      }
      siblings = node.children
    }
  }
  return roots
}

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
  const [treeFilter, setTreeFilter] = useState('')
  const [searchResults, setSearchResults] = useState<string[]>([])
  const [searching, setSearching] = useState(false)
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

  useEffect(() => {
    const q = treeFilter.trim()
    if (!q) { setSearchResults([]); return }
    const hasFullWidth = /[\u3000-\u9fff\uff00-\uffef\u3040-\u30ff\uac00-\ud7af]/.test(q)
    const minLen = hasFullWidth ? 2 : 3
    if (q.length < minLen) { setSearchResults([]); return }
    setSearching(true)
    const timer = setTimeout(() => {
      searchTreePrune(q).then(setSearchResults).catch(() => {})
      setSearching(false)
    }, 500)
    return () => clearTimeout(timer)
  }, [treeFilter])

  const handleSessionChange = useCallback((session: ChatSession | null) => {
    setActiveSession(session)
    if (session) {
      saveChatSession(session).then(refreshList).catch(() => {})
    }
  }, [refreshList])

  // 统一范围入口：把路径加入会话级检索范围，自动合并父路径吞并子路径
  const handleAddToScope = useCallback((path: string) => {
    if (!activeSession) return
    const cur = activeSession.retrieval_scope ?? []
    handleSessionChange({ ...activeSession, retrieval_scope: mergeScopePrefixes([...cur, path]) })
  }, [activeSession, handleSessionChange])

  // /范围:全库或目录路径 → 解析为路径并更新会话范围（全库→""，根目录→""，子目录→相对路径）
  const handleScopeAction = useCallback((action: string) => {
    if (!activeSession) return
    if (action === 'clear') {
      handleSessionChange({ ...activeSession, retrieval_scope: [] })
      return
    }
    if (action.startsWith('dir:')) {
      const dirName = action.slice(4)
      // 全库关键字 → 空字符串（全库检索）
      if (dirName === '全库') {
        handleAddToScope('')
        return
      }
      // 匹配 dirTrees 中 label 或 basePath 尾部命中的目录（根目录→空字符串）
      const hit = dirTrees.find(dt => dt.label === dirName || dt.basePath.endsWith('/' + dirName))
      if (hit) {
        handleAddToScope('')
      }
      // TODO: 非根子目录匹配 → 相对路径
    }
  }, [activeSession, dirTrees, handleSessionChange, handleAddToScope])

  // 树状根目录的会话范围设置：空字符串 = 全库（该监控根即为全库）
  const handleSetSessionScope = useCallback((dirId: string) => {
    const dt = dirTrees.find(d => d.id === dirId)
    if (activeSession && dt) handleAddToScope('')
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
        setActiveSession({ id, title: '', created_at: 0, updated_at: 0, messages: [], source_ids: [], source_files: [], strict_docs: true })
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
      setActiveSession({ id, title: '', created_at: 0, updated_at: 0, messages: [], source_ids: [], source_files: [], strict_docs: true })
      setActiveId(id)
      refreshList()
    } catch { /* ignore */ }
  }, [refreshList])

  const handleDelete = useCallback(async (id: string) => {
    const confirmed = await confirm(t('confirm_delete_session'), t('delete'))
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
      const content = await exportChatSessionJson(activeId)
      await saveFile(content, 'ai-chat.json')
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
    const confirmed = await confirm(
      t('confirm_delete_sessions', { n: selectedIds.size }),
      t('delete')
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
      await saveFile(combined, 'ai-chats-batch.md')
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
        <div className="border-t border-gray-200 dark:border-gray-800 flex flex-col min-h-0">
          <button
            onClick={() => setTreeExpanded(v => !v)}
            className="shrink-0 w-full px-3 py-2 flex items-center justify-between text-xs font-medium text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300 transition-colors"
          >
            <span className="flex items-center gap-1.5"><FolderIcon className="size-4 text-amber-400 dark:text-amber-500" /> <span className="text-sm">{t('file_tree')}</span></span>
            <ChevronDownIcon className={`size-3 transition-transform ${treeExpanded ? '' : '-rotate-90'}`} />
          </button>
          {treeExpanded && (
            <div className="flex-1 flex flex-col min-h-0">
              {/* 搜索过滤 */}
              <div className="px-2 py-1">
                <input
                  type="text"
                  value={treeFilter}
                  onChange={e => setTreeFilter(e.target.value)}
                  placeholder={t('file_tree_search')}
                  className="w-full text-[10px] bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded px-2 py-1 text-gray-700 dark:text-gray-300 placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500"
                />
              </div>
              <div className="flex-1 overflow-y-auto px-2 pb-2 space-y-1">
                {(() => {
                  if (!treeFilter.trim()) return dirTrees.map(dt => (
                    <div key={dt.id}>
                      <div className="px-2 py-0.5 flex items-center gap-1">
                        <span className="flex-1 text-[10px] font-medium text-gray-400 dark:text-gray-500 truncate">{dt.label}</span>
                        <button
                          type="button"
                          onClick={() => activeSession?.retrieval_scope?.includes('') ? handleClearSessionScope() : handleSetSessionScope(dt.id)}
                          title={activeSession?.retrieval_scope?.includes('') ? t('clear_session_scope') : t('set_session_scope')}
                          className={`text-[10px] ${
                            activeSession?.retrieval_scope?.includes('')
                              ? 'text-purple-600 dark:text-purple-300 font-medium'
                              : 'text-gray-400 hover:text-purple-500 dark:text-gray-500 dark:hover:text-purple-400'
                          } shrink-0`}
                        >
                          {activeSession?.retrieval_scope?.includes('') ? '范围✓' : '范围'}
                        </button>
                      </div>
                      {dt.root && sortTreeNodes(dt.root).filter(n => passesFilter(n, treeFilter)).map(child => (
                        <TreeFileList key={child.path} node={child} basePath={dt.basePath} onPick={handleTreeClick} onScope={handleAddToScope} filter={treeFilter} />
                      ))}
                    </div>
                  ));
                  const q = treeFilter.trim();
                  const hasFullWidth = /[\u3000-\u9fff\uff00-\uffef\u3040-\u30ff\uac00-\ud7af]/.test(q);
                  const minLen = hasFullWidth ? 2 : 3;
                  if (q.length < minLen) return <div className="px-2 py-4 text-center text-[10px] text-gray-400">{t('file_tree_min_chars', { n: minLen })}</div>;
                  if (searching) return <div className="px-2 py-4 text-center text-[10px] text-gray-400">搜索中…</div>;
                  if (searchResults.length > 0) {
                    const tree = buildResultTree(searchResults);
                    const render = (nodes: ResultNode[], depth: number) => nodes.map(n => (
                      <div key={n.path}>
                        <div
                          style={{ paddingLeft: `${depth * 12}px` }}
                          className={n.isMatch
                            ? "flex items-center gap-1.5 py-1 text-xs text-gray-700 dark:text-gray-200 bg-blue-50 dark:bg-blue-950/30 hover:bg-blue-100 dark:hover:bg-blue-900/40 rounded cursor-pointer"
                            : "flex items-center gap-1.5 py-0.5 text-[10px] text-gray-400 dark:text-gray-500"}
                          onClick={n.isMatch ? () => { setPendingMention(n.path); setTreeFilter(''); setSearchResults([]) } : undefined}
                          onContextMenu={n.isMatch ? (e) => { e.preventDefault(); handleAddToScope(n.path) } : undefined}
                          title={n.isMatch ? `${t('file_tree_file_hint')}${n.path}` : undefined}
                        >
                          <span className="shrink-0">{n.children.length > 0 || !n.name.includes('.') ? <FolderOpenIcon className="size-3 text-amber-400 dark:text-amber-500" /> : <FileTextIcon className="size-3 text-gray-400 dark:text-gray-500" />}</span>
                          <span className={`truncate flex-1 ${n.isMatch ? 'font-medium' : ''}`}>{n.name}</span>
                          {n.isMatch && <span className="shrink-0 text-[9px] text-gray-400">+ 范围</span>}
                        </div>
                        {n.children.length > 0 && render(n.children, depth + 1)}
                      </div>
                    ));
                    return render(tree, 0);
                  }
                  return <div className="px-2 py-4 text-center text-[10px] text-gray-400">{t('file_tree_search_no_results')}</div>;
                })()}
                {dirTrees.length === 0 && (
                  <div className="px-2 py-1 text-[10px] text-gray-400">{t('no_dirs')}</div>
                )}
              </div>
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

function passesFilter(node: DirTreeNode, filter: string): boolean {
  if (!filter.trim()) return true
  const q = filter.toLowerCase()
  if (node.name.toLowerCase().includes(q)) return true
  if (node.path.toLowerCase().includes(q)) return true
  if (node.children.length > 0) {
    return node.children.some(c => passesFilter(c, q))
  }
  return false
}

function TreeFileList({ node, basePath, onPick, onScope, filter }: {
  node: DirTreeNode
  basePath: string
  onPick: (relPath: string) => void
  onScope?: (relPath: string) => void
  filter?: string
}) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [children, setChildren] = useState<DirTreeNode[] | null>(
    node.children.length > 0 ? node.children : null,
  )
  const [loading, setLoading] = useState(false)
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null)
  const isDir = node.is_dir || node.children.length > 0
  const rel = node.path.startsWith(basePath)
    ? node.path.slice(basePath.length).replace(/^\/+/, '')
    : node.path

  const sortedChildren = children ? sortTreeNodes(children) : null
  const filteredChildren = sortedChildren && filter ? sortedChildren.filter(c => passesFilter(c, filter)) : sortedChildren
  const showMatch = filter && node.name.toLowerCase().includes(filter.toLowerCase())

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

  // 点击外部关闭右键菜单
  useEffect(() => {
    if (!contextMenu) return
    const close = () => setContextMenu(null)
    document.addEventListener('click', close)
    return () => document.removeEventListener('click', close)
  }, [contextMenu])

  return (
    <div>
      <div className="group relative">
        <button
          type="button"
          draggable
          onDragStart={e => { e.dataTransfer.setData('text/plain', rel); e.dataTransfer.effectAllowed = 'copy' }}
          onClick={isDir ? handleToggle : () => onPick(rel)}
          onContextMenu={e => {
            e.preventDefault()
            setContextMenu({ x: e.clientX, y: e.clientY })
          }}
          className={`w-full flex items-center gap-1.5 px-2 py-1 text-sm rounded transition-colors ${
            showMatch ? 'bg-yellow-50 dark:bg-yellow-900/10' : 'hover:bg-gray-100 dark:hover:bg-gray-800'
          } text-gray-700 dark:text-gray-200`}
          title={isDir ? `${t('file_tree_dir_hint')}${rel}` : `${t('file_tree_file_hint')}${rel}`}
        >
          {/* 索引状态标识 */}
          {!isDir && (
            <span className={`shrink-0 size-1.5 rounded-full ${
              node.indexed === true ? 'bg-green-500' : node.indexed === false ? 'bg-gray-300 dark:bg-gray-600' : 'bg-transparent'
            }`} />
          )}
          {isDir ? (loading ? <LoadingSpinner className="size-3.5 shrink-0 text-gray-400" /> : open ? <FolderOpenIcon className="size-3.5 shrink-0 text-amber-400 dark:text-amber-500" /> : <FolderIcon className="size-3.5 shrink-0 text-amber-400 dark:text-amber-500" />) : <FileTextIcon className="size-3.5 shrink-0 text-gray-400 dark:text-gray-500" />}
          <span className="truncate text-xs">{node.name}</span>
          {showMatch && <span className="ml-1 text-[9px] text-yellow-600 dark:text-yellow-400 shrink-0">✓</span>}
          {!isDir && node.indexed === false && (
            <span className="ml-auto text-[9px] text-gray-400 dark:text-gray-600 shrink-0">{t('file_tree_unindexed')}</span>
          )}
        </button>
        {/* 右键菜单 */}
        {contextMenu && (
          <div
            className="fixed z-50 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded shadow-lg py-1 text-xs"
            style={{ left: contextMenu.x, top: contextMenu.y }}
          >
            <button
              className="w-full px-3 py-1.5 text-left text-gray-700 dark:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700"
              onClick={() => { onScope?.(rel); setContextMenu(null) }}
            >
              {t('file_tree_menu_scope')}
            </button>
            {!isDir && (
              <button
                className="w-full px-3 py-1.5 text-left text-gray-700 dark:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700"
                onClick={() => { onPick(rel); setContextMenu(null) }}
              >
                {t('file_tree_menu_mention')}
              </button>
            )}
          </div>
        )}
      </div>
      {open && filteredChildren && filteredChildren.map(child => (
        <div key={child.path} className="pl-3">
          <TreeFileList key={child.path} node={child} basePath={basePath} onPick={onPick} onScope={onScope} filter={filter} />
        </div>
      ))}
    </div>
  )
}