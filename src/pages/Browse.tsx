import { useEffect, useState, useCallback, useRef } from 'react'
import { useSearchParams } from 'react-router-dom'
import { invoke } from '@tauri-apps/api/core'
import { ask, save } from '@tauri-apps/plugin-dialog'
import { writeTextFile } from '@tauri-apps/plugin-fs'
import { useI18n } from '../i18n'
import { type FilePreview, openFile, revealInFolder, askDocuments, aiCapabilities, type AiCapabilities, previewFile } from '../api/files'
import { type FileItem, type FilterType, type SortKey, type SortOrder, listFilesDb, getBrowseFileTypes } from '../api/files'
import { reindexFiles } from '../api/index'
import { LoadingSpinner, SearchIcon } from '../icons'
import { usePersistentState } from '../hooks/usePersistentState'
import { useSearch as useFtsSearch } from '../hooks/useSearch'
import SearchBar from '../components/SearchBar'
import ResultList from '../components/ResultList'
import type { SearchHit } from '../api/search'

function CopyAllButton({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <button
      onClick={() => {
        navigator.clipboard.writeText(text).then(() => {
          setCopied(true)
          setTimeout(() => setCopied(false), 1500)
        }).catch(e => console.warn('复制失败:', e))
      }}
      className="shrink-0 px-2 py-0.5 text-[10px] font-medium text-gray-500 dark:text-gray-400 bg-gray-100 dark:bg-gray-800 rounded hover:bg-gray-200 dark:hover:bg-gray-700 transition-colors"
      title={label}
    >
      {copied ? '✓' : label}
    </button>
  )
}

const LS_KEY_FILTER = 'ls_browse_filter'
const LS_KEY_EXT = 'ls_browse_ext'
const LS_KEY_SEARCH = 'ls_browse_search'
const LS_KEY_SORT = 'ls_browse_sort'
const LS_KEY_ORDER = 'ls_browse_order'
const LS_KEY_COLS = 'ls_browse_cols'

function statusBadge(indexed: number, error_msg: string | null | undefined, t: (k: string) => string) {
  if (indexed === 1) return <span className="inline-flex items-center gap-1 text-xs text-green-600 dark:text-green-400">✓ {t('indexed')}</span>
  if (indexed === 3) return <span className="inline-flex items-center gap-1 text-xs text-yellow-600 dark:text-yellow-400" title={t('extracted_but_not_indexed')}>◐ {t('indexing')}</span>
  if (indexed === 2) return <span className="inline-flex items-center gap-1 text-xs text-red-600 dark:text-red-400" title={error_msg ?? undefined}>✗ {t('failed')}</span>
  return <span className="inline-flex items-center gap-1 text-xs text-yellow-600 dark:text-yellow-400">○ {t('pending')}</span>
}

export default function Browse() {
  const { t } = useI18n()
  const [params, setParams] = useSearchParams()
  // 深链路径：挂载时若包含 ?path=，作为优先搜索词（不污染持久化的 search）
  const [forcedSearch, setForcedSearch] = useState<string | null>(() => params.get('path'))
  const [items, setItems] = useState<FileItem[]>([])
  const [total, setTotal] = useState(0)
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(20)
  const [filter, setFilter] = usePersistentState<FilterType>(LS_KEY_FILTER, params.get('filter') as FilterType || 'all')
  const [ext, setExt] = usePersistentState<string>(LS_KEY_EXT, params.get('ext') || '')
  const [availableExts, setAvailableExts] = useState<string[]>([])
  const [search, setSearch] = usePersistentState<string>(LS_KEY_SEARCH, params.get('search') || '')
  const [debouncedSearch, setDebouncedSearch] = useState(search)
  const [sort, setSort] = usePersistentState<SortKey>(LS_KEY_SORT, params.get('sort') as SortKey || 'name')
  const [order, setOrder] = usePersistentState<SortOrder>(LS_KEY_ORDER, params.get('order') as SortOrder || 'asc')
  const [loading, setLoading] = useState(false)
  const [selectedFile, setSelectedFile] = useState<string | null>(null)
  const [preview, setPreview] = useState<FilePreview | null>(null)
  const [previewLoading, setPreviewLoading] = useState(false)
  const [previewError, setPreviewError] = useState<string | null>(null)
  const [previewCollapsed, setPreviewCollapsed] = useState(false)
  const [previewZoom, setPreviewZoom] = useState(1)
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; item: FileItem } | null>(null)
  const [indexLog, setIndexLog] = useState<string | null>(null)
  const [indexLogLoading, setIndexLogLoading] = useState(false)
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set())
  const [lastClickedIdx, setLastClickedIdx] = useState<number | null>(null)
  const [askQuestion, setAskQuestion] = useState('')
  const [aiCap, setAiCap] = useState<AiCapabilities>({ embedding: false, llm: false })
  const [askAnswer, setAskAnswer] = useState<string | null>(null)
  const [askError, setAskError] = useState(false)
  useEffect(() => { aiCapabilities().then(setAiCap).catch(() => {}) }, [])
  const [askLoading, setAskLoading] = useState(false)
  // 全文搜索模式（与 SearchPage 共享 useSearch hook）
  const fts = useFtsSearch()
  const [selectedSearchHit, setSelectedSearchHit] = useState<SearchHit | null>(null)
  type ColKey = 'filename' | 'path' | 'type' | 'status'
  const [colWidths, setColWidths] = usePersistentState<Record<ColKey, number>>(LS_KEY_COLS, { filename: 192, path: 200, type: 64, status: 112 })
  const resizingRef = useRef<{ col: ColKey; startX: number; startWidth: number } | null>(null)
  const tableRef = useRef<HTMLDivElement>(null)
  const rowHeightRef = useRef<number | null>(null)
  // Fallback row height while no data row rendered yet (loading / empty):
  // text-xs line-height 16px + py-1 padding 8px + 1px row border = 25px.
  // ponytail: fixed estimate — drifts if row font/padding changes. Upgrade:
  // render a hidden probe row (or read computed styles) once at mount.
  const EST_ROW_HEIGHT = 25

  useEffect(() => {
    const el = tableRef.current
    if (!el) return
    const measure = () => {
      const row = el.querySelector<HTMLTableRowElement>('tbody tr')
      if (row) rowHeightRef.current = row.getBoundingClientRect().height
      const rowH = rowHeightRef.current ?? EST_ROW_HEIGHT
      setPageSize(Math.min(1000, Math.max(1, Math.floor(el.clientHeight / rowH))))
    }
    measure()
    const ro = new ResizeObserver(measure)
    ro.observe(el)
return () => ro.disconnect()
  }, [])

  // Rows render only after data loads, so the real height can't be read on mount.
  useEffect(() => {
    const el = tableRef.current
    if (!el) return
    const row = el.querySelector<HTMLTableRowElement>('tbody tr')
    if (row) {
      rowHeightRef.current = row.getBoundingClientRect().height
      setPageSize(Math.min(1000, Math.max(1, Math.floor(el.clientHeight / rowHeightRef.current))))
    }
  }, [items])

  useEffect(() => {
    const close = () => setContextMenu(null)
    document.addEventListener('click', close)
return () => document.removeEventListener('click', close)
  }, [])

  const handleResizeStart = useCallback((e: React.MouseEvent, col: ColKey) => {
    e.preventDefault()
    resizingRef.current = { col, startX: e.clientX, startWidth: colWidths[col] }

    const onMouseMove = (ev: MouseEvent) => {
      const r = resizingRef.current
      if (!r) return
      const newWidth = Math.max(80, r.startWidth + (ev.clientX - r.startX))
      setColWidths(prev => ({ ...prev, [r.col]: newWidth }))
    }
    const onMouseUp = () => {
      resizingRef.current = null
      document.removeEventListener('mousemove', onMouseMove)
      document.removeEventListener('mouseup', onMouseUp)
    }
    document.addEventListener('mousemove', onMouseMove)
    document.addEventListener('mouseup', onMouseUp)
  }, [colWidths])

  const handleAutoFit = useCallback((col: ColKey) => {
    const colIdx = ({ filename: 0, path: 1, type: 2, status: 3 } as const)[col]
    const cells = tableRef.current?.querySelectorAll<HTMLTableCellElement>(
      `tbody td:nth-child(${colIdx + 1})`
    )
    if (!cells || cells.length === 0) return
    const ruler = document.createElement('span')
    Object.assign(ruler.style, {
      visibility: 'hidden', position: 'absolute', whiteSpace: 'nowrap',
      fontSize: '0.75rem', lineHeight: '1rem',
    })
    document.body.appendChild(ruler)
    let max = 80
    cells.forEach(cell => {
      const el = cell.querySelector<HTMLElement>('[title]')
      ruler.textContent = el?.getAttribute('title') ?? cell.textContent ?? ''
      max = Math.max(max, ruler.offsetWidth + 20)
    })
    document.body.removeChild(ruler)
    setColWidths(prev => ({ ...prev, [col]: Math.min(max, 600) }))
  }, [])


  // Sync URL params
  useEffect(() => {
    const p = new URLSearchParams()
    if (filter !== 'all') p.set('filter', filter)
    if (ext) p.set('ext', ext)
    if (search) p.set('search', search)
    if (sort !== 'name') p.set('sort', sort)
    if (order !== 'asc') p.set('order', order)
    setParams(p, { replace: true })
  }, [filter, ext, search, sort, order, setParams])

  // Load available file types dynamically; reset stale ext param
  useEffect(() => {
    getBrowseFileTypes()
      .then(types => {
        setAvailableExts(types)
        if (ext && !types.includes(ext)) setExt('')
      })
      .catch(() => setAvailableExts([]))
  }, [])

  const loadFiles = useCallback(async () => {
    setLoading(true)
    try {
      const res = await listFilesDb({ filter, ext: ext || undefined, search: (forcedSearch ?? debouncedSearch) || undefined, sort, order, page, pageSize })
      setItems(res.items)
      setTotal(res.total)
    } catch {
      setItems([])
      setTotal(0)
    } finally {
      setLoading(false)
    }
  }, [filter, ext, forcedSearch, debouncedSearch, sort, order, page, pageSize])

  // Clamp page to a valid range when the result set shrinks (e.g. after a
  // re-scan fixes failures), so the user is never left on an empty page.
  const totalPages = Math.max(1, Math.ceil(total / pageSize))
  useEffect(() => {
    if (page > totalPages) setPage(totalPages)
  }, [page, totalPages])

  const viewIndexLog = useCallback(async (fileId: string) => {
    setIndexLogLoading(true)
    setIndexLog(null)
    try {
      const lines: string[] = await invoke('get_logs', { lines: 500, fileId })
      setIndexLog(lines.join('\n') || t('no_index_log_for_file'))
    } catch {
      setIndexLog(t('failed_load_index_log'))
    } finally {
      setIndexLogLoading(false)
    }
  }, [])

  const handleAsk = useCallback(async () => {
    if (!askQuestion.trim() || askLoading) return
    if (selectedIds.size === 0) return
    setAskLoading(true)
    setAskAnswer(null)
    setAskError(false)
    try {
      const answer = await askDocuments([...selectedIds], askQuestion.trim())
      setAskAnswer(answer)
    } catch (e) {
      setAskAnswer(e instanceof Error ? e.message : t('ai_ask_failed'))
      setAskError(true)
    } finally {
      setAskLoading(false)
    }
  }, [askQuestion, askLoading, selectedIds])

  useEffect(() => {
    const t = setTimeout(() => setDebouncedSearch(search), 300)
return () => clearTimeout(t)
  }, [search])


  useEffect(() => {
    loadFiles()
  }, [loadFiles])

  // Also sync filter to URL on mount
  useEffect(() => {
    const f = params.get('filter')
    if (f) setFilter(f as FilterType)
  }, [])

  const previewVersionRef = useRef(0)
  const selectFile = useCallback(async (path: string) => {
    const localVersion = ++previewVersionRef.current
    setSelectedFile(path)
    setPreviewLoading(true)
    setPreviewError(null)
    setPreview(null)
    try {
      const result: FilePreview = await invoke('preview_file_by_path', { path })
      if (previewVersionRef.current !== localVersion) return
      setPreview(result)
    } catch (e) {
      if (previewVersionRef.current !== localVersion) return
      setPreviewError(typeof e === 'string' ? e : t('failed_load_preview'))
    }
    if (previewVersionRef.current !== localVersion) return
    setPreviewLoading(false)
  }, [])
  // 搜索结果选中时同步到预览面板
  const handleSearchSelect = useCallback((hit: SearchHit) => {
    setSelectedSearchHit(hit)
    selectFile(hit.path)
  }, [selectFile])

  // 键盘导航：↑↓ 移动选中行，Enter 打开预览；Ctrl/Cmd+A 全选当前页
  // 键盘导航：↑↓ 移动选中行；Ctrl/Cmd+A 全选当前页
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const isSearch = document.activeElement?.closest('[data-search-input]') || document.activeElement?.tagName === 'INPUT'
      if (isSearch) return
      if (items.length === 0) return
      if ((e.metaKey || e.ctrlKey) && e.key === 'a') {
        e.preventDefault()
        setSelectedIds(new Set(items.map(i => i.file_id)))
        return
      }
      // 定位当前焦点行（lastClickedIdx 优先，否则用 selectedIds 首个）
      let idx = lastClickedIdx ?? items.findIndex(i => selectedIds.has(i.file_id))
      if (idx < 0) idx = 0
      if (e.key === 'ArrowDown') {
        e.preventDefault()
        const next = Math.min(idx + 1, items.length - 1)
        const item = items[next]
        selectFile(item.rel_path)
        setSelectedIds(new Set([item.file_id]))
        setLastClickedIdx(next)
        tableRef.current?.querySelector(`tr[data-relpath="${CSS.escape(item.rel_path)}"]`)?.scrollIntoView({ block: 'nearest' })
      } else if (e.key === 'ArrowUp') {
        e.preventDefault()
        const prev = Math.max(idx - 1, 0)
        const item = items[prev]
        selectFile(item.rel_path)
        setSelectedIds(new Set([item.file_id]))
        setLastClickedIdx(prev)
        tableRef.current?.querySelector(`tr[data-relpath="${CSS.escape(item.rel_path)}"]`)?.scrollIntoView({ block: 'nearest' })
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [items, selectedIds, lastClickedIdx, selectFile])
  useEffect(() => {
    if (forcedSearch) {
      selectFile(forcedSearch)
    }
  }, [selectFile, forcedSearch])

  useEffect(() => {
    if (forcedSearch && items.length > 0) {
      const row = tableRef.current?.querySelector(`tr[data-relpath="${CSS.escape(forcedSearch)}"]`)
      if (row) {
        row.scrollIntoView({ block: 'nearest' })
      }
    }
  }, [items, forcedSearch])

return (
    <div className="flex h-full">
      {/* Left: Table / Search Results */}
      <div className="flex-1 flex flex-col min-w-0 border-r border-gray-200 dark:border-gray-800">
        {/* Toolbar: browse mode + search bar */}
        <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-800 flex items-center gap-3 flex-wrap">
          <div className="flex-1 flex items-center gap-3">
            <SearchBar
              query={fts.query}
              loading={fts.status === 'loading'}
              onQueryChange={fts.setQuery}
              onSubmit={fts.submitSearch}
            />
            {fts.query && (
              <button
                onClick={() => { fts.setQuery(''); fts.submitSearch() }}
                className="text-xs px-2 py-1 text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 shrink-0"
                title={t('clear_search')}
              >
                ×
              </button>
            )}
          </div>
          {!fts.query && (<>
          <select
            value={filter}
            onChange={e => { setFilter(e.target.value as FilterType); setPage(1) }}
            className="text-xs bg-transparent border border-gray-200 dark:border-gray-700 rounded px-2 py-1 text-gray-600 dark:text-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500"
          >
            <option value="all">{t('all')}</option>
            <option value="indexed">{t('indexed')}</option>
            <option value="pending">{t('pending')}</option>
            <option value="failed">{t('failed')}</option>
          </select>

          <select
            value={ext}
            onChange={e => { setExt(e.target.value); setPage(1) }}
            className="text-xs bg-transparent border border-gray-200 dark:border-gray-700 rounded px-2 py-1 text-gray-600 dark:text-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500"
          >
            <option value="">{t('all_types')}</option>
            {availableExts.map(e => (
              <option key={e} value={e}>{e.toUpperCase()}</option>
            ))}
          </select>

          <div className="relative flex-1 min-w-[160px] max-w-xs">
            <input
              type="text"
              value={search}
              onChange={e => { setForcedSearch(null); setSearch(e.target.value); setPage(1) }}
              onKeyDown={e => e.key === 'Enter' && setPage(1)}
              placeholder={t('search_filename')}
              className="w-full text-xs bg-transparent border border-gray-200 dark:border-gray-700 rounded px-2 py-1 pr-7 text-gray-700 dark:text-gray-300 placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500"
            />
            {search && (
              <button onClick={() => { setSearch(''); setPage(1) }} className="absolute right-1.5 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                ×
              </button>
            )}
          </div>

          <select
            value={`${sort}-${order}`}
            onChange={e => {
              const [s, o] = e.target.value.split('-') as [SortKey, SortOrder]
              setSort(s)
              setOrder(o)
              setPage(1)
            }}
            className="text-xs bg-transparent border border-gray-200 dark:border-gray-700 rounded px-2 py-1 text-gray-600 dark:text-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500"
          >
            <option value="name-asc">{t('name_az')}</option>
            <option value="name-desc">{t('name_za')}</option>
            <option value="size-desc">{t('size_desc')}</option>
            <option value="size-asc">{t('size_asc')}</option>
            <option value="mtime-desc">{t('newest')}</option>
            <option value="mtime-asc">{t('oldest')}</option>
            <option value="ext-asc">{t('ext_az')}</option>
          </select>

          <button
            onClick={() => loadFiles()}
            disabled={loading}
            className="text-xs px-2 py-1 text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 disabled:opacity-50"
            title={t('refresh')}
          >
            ↻
          </button>

          <span className="text-xs text-gray-400 dark:text-gray-500 ml-auto">{total.toLocaleString()} {t('files')}</span>
          </>)}
        </div>

        {fts.query ? (
          /* 搜索模式：全文搜索结果 */
          <>
            <div className="flex-1 overflow-y-auto">
              {fts.status === 'loading' && (
                <div className="flex items-center justify-center py-16">
                  <LoadingSpinner className="size-5" />
                </div>
              )}
              {fts.status === 'error' && (
                <div className="flex flex-col items-center justify-center py-16 px-4">
                  <p className="text-sm text-red-600 dark:text-red-400 mb-3">{fts.error}</p>
                  <button onClick={fts.retry} className="px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors">{t('retry')}</button>
                </div>
              )}
              {fts.status === 'success' && (
                <ResultList hits={fts.hits} selectedId={selectedSearchHit?.file_id ?? null} onSelect={handleSearchSelect} />
              )}
              {fts.status === 'success' && fts.hits.length === 0 && (
                <div className="flex flex-col items-center justify-center py-16 px-4 text-center space-y-3">
                  <p className="text-lg font-medium text-gray-900 dark:text-gray-100">{t('no_results_found')}</p>
                  <p className="text-sm text-gray-500 dark:text-gray-400">{t('no_results_hint')}</p>
                </div>
              )}
              {fts.status === 'idle' && (
                <div className="flex items-center justify-center h-full text-sm text-gray-400 dark:text-gray-500">
                  <span className="flex items-center gap-2"><SearchIcon className="size-5" /> {t('search_your_documents')}</span>
                </div>
              )}
            </div>
            {fts.status === 'success' && (() => {
              const totalPages = Math.max(1, Math.ceil(fts.total / fts.pageSize))
              return totalPages > 1 ? (
                <div className="flex items-center justify-center gap-2 px-4 py-3 border-t border-gray-100 dark:border-gray-800">
                  <button onClick={() => fts.setPage(fts.page - 1)} disabled={fts.page <= 1}
                    className="px-3 py-1 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed transition-colors">
                    {t('prev_page')}
                  </button>
                  <span className="text-xs text-gray-500 dark:text-gray-400">{t('results_count', { total: fts.total })}</span>
                  <button onClick={() => fts.setPage(fts.page + 1)} disabled={fts.page >= totalPages}
                    className="px-3 py-1 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed transition-colors">
                    {t('next_page')}
                  </button>
                </div>
              ) : null
            })()}
          </>
        ) : (
          /* 浏览模式：文件表格 */
          <>
            <div className="flex-1 overflow-auto" ref={tableRef}>
          {loading ? (
            <div className="flex items-center justify-center py-16">
              <LoadingSpinner className="size-5" />
            </div>
          ) : (
            <table className="w-full text-xs select-none table-fixed">
              <thead className="sticky top-0 bg-gray-50 dark:bg-gray-900/80 backdrop-blur z-10">
                <tr className="border-b border-gray-200 dark:border-gray-800 text-gray-500 dark:text-gray-400 text-left">
                  <th className="px-2 py-1 font-medium relative" style={{ width: colWidths.filename }}>
                    {t('filename')}
                    <div className="absolute right-0 top-0 bottom-0 w-1.5 cursor-col-resize hover:bg-blue-500/50 active:bg-blue-500" onMouseDown={(e) => handleResizeStart(e, 'filename')} onDoubleClick={() => handleAutoFit('filename')} />
                  </th>
                  <th className="px-2 py-1 font-medium relative" style={{ width: colWidths.path }}>
                    {t('path')}
                    <div className="absolute right-0 top-0 bottom-0 w-1.5 cursor-col-resize hover:bg-blue-500/50 active:bg-blue-500" onMouseDown={(e) => handleResizeStart(e, 'path')} onDoubleClick={() => handleAutoFit('path')} />
                  </th>
                  <th className="px-2 py-1 font-medium relative" style={{ width: colWidths.type }}>
                    {t('type')}
                    <div className="absolute right-0 top-0 bottom-0 w-1.5 cursor-col-resize hover:bg-blue-500/50 active:bg-blue-500" onMouseDown={(e) => handleResizeStart(e, 'type')} onDoubleClick={() => handleAutoFit('type')} />
                  </th>
                  <th className="px-2 py-1 font-medium relative" style={{ width: colWidths.status }}>
                    {t('status')}
                    <div className="absolute right-0 top-0 bottom-0 w-1.5 cursor-col-resize hover:bg-blue-500/50 active:bg-blue-500" onMouseDown={(e) => handleResizeStart(e, 'status')} onDoubleClick={() => handleAutoFit('status')} />
                  </th>
                </tr>
              </thead>
              <tbody>
                {items.map((item, idx) => (
                  <tr
                    key={item.file_id}
                    data-relpath={item.rel_path}
                    onClick={(e) => {
                      if (e.metaKey || e.ctrlKey) {
                        selectFile(item.rel_path)
                        setSelectedIds(prev => {
                          const next = new Set(prev)
                          if (next.has(item.file_id)) next.delete(item.file_id)
                          else next.add(item.file_id)
                          return next
                        })
                        setLastClickedIdx(idx)
                      } else if (e.shiftKey && lastClickedIdx !== null) {
                        const lo = Math.min(lastClickedIdx, idx)
                        const hi = Math.max(lastClickedIdx, idx)
                        const range = new Set(items.slice(lo, hi+1).map(i => i.file_id))
                        setSelectedIds(range)
                      } else {
                        selectFile(item.rel_path)
                        setSelectedIds(new Set([item.file_id]))
                        setLastClickedIdx(idx)
                      }
                    }}
                    onContextMenu={(e) => {
                      e.preventDefault()
                      // Right-clicking a row that isn't selected selects it alone.
                      if (!selectedIds.has(item.file_id)) {
                        setSelectedIds(new Set([item.file_id]))
                      }
                      setContextMenu({ x: e.clientX, y: e.clientY, item })
                    }}
                    className={`border-b border-gray-100 dark:border-gray-800/50 cursor-pointer transition-colors ${
                      selectedIds.has(item.file_id)
                        ? 'bg-blue-50 dark:bg-blue-900/20'
                        : selectedFile === item.rel_path
                        ? 'bg-blue-50 dark:bg-blue-900/20'
                        : 'hover:bg-gray-50 dark:hover:bg-gray-800/50'
                    }`}
                  >
                    <td className="px-2 py-1">
                      <div className="flex items-center gap-2">
                        <span className={`size-2 rounded-full shrink-0 ${
                          item.indexed === 1 ? 'bg-green-500' : item.indexed === 2 ? 'bg-red-500' : 'bg-yellow-500'
                        }`} />
                        <span className="truncate" title={item.file_name}>{item.file_name}</span>
                      </div>
                    </td>
                    <td className="px-2 py-1">
                      <span className="text-gray-500 dark:text-gray-400 truncate block" title={item.rel_path}>{item.rel_path}</span>
                    </td>
                    <td className="px-2 py-1">
                      <span className="text-gray-500 dark:text-gray-400 uppercase">{item.file_ext || '—'}</span>
                    </td>
                    <td className="px-2 py-1">{statusBadge(item.indexed, item.error_msg, t)}</td>
                  </tr>
                ))}
                {!loading && items.length === 0 && (
                  <tr>
                    <td colSpan={4} className="px-4 py-16 text-center text-gray-400 dark:text-gray-500">
                      {t('no_files_found')}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          )}
        </div>

        {/* AI Q&A bar */}
        <div className="px-4 py-2 border-t border-gray-200 dark:border-gray-800">
          <div className="flex items-center gap-2">
            <input
              type="text"
              value={askQuestion}
              onChange={e => setAskQuestion(e.target.value)}
              onKeyDown={e => { if (e.key === 'Enter' && aiCap.llm) handleAsk() }}
              disabled={!aiCap.llm}
              placeholder={aiCap.llm
                ? (selectedIds.size > 0 ? t('ask_selected', { n: selectedIds.size }) : t('ask_select_files'))
                : t('ai_llm_unavailable')}
              className="flex-1 px-3 py-1.5 text-xs border border-gray-200 dark:border-gray-700 rounded bg-gray-50 dark:bg-gray-800 text-gray-700 dark:text-gray-300 placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-purple-500 disabled:opacity-40"
            />
            <button
              onClick={handleAsk}
              disabled={askLoading || !aiCap.llm || selectedIds.size === 0 || !askQuestion.trim()}
              className="px-3 py-1.5 text-xs font-medium text-white bg-purple-600 hover:bg-purple-700 rounded disabled:opacity-40 disabled:cursor-not-allowed transition-colors shrink-0"
            >
              {askLoading ? <LoadingSpinner className="size-3.5" /> : t('ask_ai')}
            </button>
          </div>
          {askAnswer && (
            <div className={`mt-2 text-xs rounded p-2.5 ${
              askError
                ? 'text-red-700 dark:text-red-300 bg-red-50 dark:bg-red-900/10 border border-red-100 dark:border-red-800/30'
                : 'text-gray-700 dark:text-gray-300 bg-purple-50 dark:bg-purple-900/10 border border-purple-100 dark:border-purple-800/30'
            }`}>
              {askAnswer}
            </div>
          )}
        </div>

        {/* Pagination */}
        <div className="px-4 py-2 border-t border-gray-200 dark:border-gray-800 flex items-center justify-between">
          <button
            onClick={() => setPage(p => Math.max(1, p - 1))}
            disabled={page <= 1}
            className="px-3 py-1 text-xs border border-gray-200 dark:border-gray-700 rounded hover:bg-gray-100 dark:hover:bg-gray-800 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
          >
            ← {t('prev_page')}
          </button>
          <div className="flex items-center gap-2">
            <span className="text-xs text-gray-500 dark:text-gray-400">{t('go_to')}</span>
            <input
              type="number"
              value={page}
              onChange={(e) => {
                const p = parseInt(e.target.value, 10)
                if (!isNaN(p) && p >= 1 && p <= totalPages) setPage(p)
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault()
                  const input = e.target as HTMLInputElement
                  const p = parseInt(input.value, 10)
                  if (!isNaN(p) && p >= 1 && p <= totalPages) { setPage(p); input.blur() }
                }
              }}
              className="w-16 px-2 py-1 text-xs border border-gray-200 dark:border-gray-700 rounded bg-gray-50 dark:bg-gray-800 text-gray-600 dark:text-gray-400 text-center focus:outline-none focus:ring-2 focus:ring-blue-500"
              min={1}
              max={totalPages}
            />
          </div>
          <span className="text-xs text-gray-500 dark:text-gray-400">
            {t('page_info', { page, total: totalPages, start: ((page - 1) * pageSize) + 1, end: Math.min(page * pageSize, total), totalAll: total })}
          </span>
          <button
            onClick={() => setPage(p => Math.min(totalPages, p + 1))}
            disabled={page >= totalPages}
            className="px-3 py-1 text-xs border border-gray-200 dark:border-gray-700 rounded hover:bg-gray-100 dark:hover:bg-gray-800 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
          >
            {t('next_page')} →
          </button>
        </div>
          </>)}

      </div>

      {/* Right: Preview */}
      {!previewCollapsed && (
      <div className="w-80 shrink-0 overflow-y-auto bg-white dark:bg-gray-900">
        {previewLoading && (
          <div className="flex items-center justify-center py-16">
            <LoadingSpinner className="size-5" />
          </div>
        )}
        {previewError && (
          <div className="p-4 text-sm text-red-600 dark:text-red-400">
            {previewError}
          </div>
        )}
        {preview && !previewLoading && (
           <div className="p-4 min-h-full">
            {selectedFile && (
              <div className="mb-3 flex items-center justify-between gap-2">
                <span className="text-xs font-medium text-gray-700 dark:text-gray-200 truncate" title={selectedFile}>{selectedFile}</span>
                {preview.content && (
                  <CopyAllButton text={preview.content} label={t('copy_all_text')} />
                )}
                {preview.file_type === 'image' && (
                  <span className="flex items-center gap-1 shrink-0">
                    {[0.5, 1, 1.5, 2].map(z => (
                      <button
                        key={z}
                        onClick={() => setPreviewZoom(z)}
                        className={`px-1.5 py-0.5 text-[10px] rounded border transition-colors ${
                          previewZoom === z
                            ? 'bg-blue-500 text-white border-blue-500'
                            : 'text-gray-500 dark:text-gray-400 border-gray-200 dark:border-gray-700 hover:bg-gray-100 dark:hover:bg-gray-700'
                        }`}
                      >
                        {z * 100}%
                      </button>
                    ))}
                  </span>
                )}
              </div>
            )}
            {preview.file_type === 'image' && preview.image_path && (
              <div className="mb-3 flex justify-center overflow-hidden">
                <img
                  src={preview.image_base64 ? (() => {
            const ext = (preview.image_path!.split(".").pop() || "jpeg").toLowerCase()
            const mime = { jpg: "jpeg", jpeg: "jpeg", png: "png", gif: "gif", webp: "webp", bmp: "bmp", tiff: "tiff", tif: "tiff" }[ext] || "jpeg"
            return "data:image/" + mime + ";base64," + preview.image_base64
          })() : ""}
                  alt=""
                  style={{ transform: `scale(${previewZoom})`, transformOrigin: 'top left' }}
                  className={`max-w-full object-contain rounded-lg border border-gray-200 dark:border-gray-700 transition-transform ${previewZoom > 1 ? 'max-h-none' : 'max-h-64'}`}
                />
              </div>
            )}
            {preview.content && (
              <pre className="text-xs text-gray-700 dark:text-gray-300 whitespace-pre-wrap font-mono leading-relaxed overflow-y-auto max-h-[calc(100vh-16rem)]">
                {preview.content.length > 50000 ? preview.content.slice(0, 50000) + '…' : preview.content}
              </pre>
            )}
            {preview.content && preview.content.length > 50000 && (
              <p className="mt-1 text-xs text-amber-600 dark:text-amber-400">
                {t('truncated_notice')}
              </p>
            )}
            {!preview.content && preview.file_type !== 'image' && (
              <p className="text-sm text-gray-400 dark:text-gray-500">{t('no_preview_available')}</p>
            )}
            {preview.char_count > 0 && (
              <p className="mt-3 text-xs text-gray-400 border-t border-gray-200 dark:border-gray-800 pt-2">
                {t('characters_count', { count: preview.char_count })} {preview.ocr_used ? '(OCR)' : ''}
              </p>
            )}
          </div>
        )}
        {!preview && !previewLoading && !previewError && selectedFile && (
          <p className="p-4 text-sm text-gray-400 dark:text-gray-500">{t('loading_preview')}</p>
        )}
        {!selectedFile && !previewLoading && (
          <div className="flex items-center justify-center h-full text-sm text-gray-400 dark:text-gray-500">
            {t('select_file_preview')}
          </div>
        )}
      </div>
      )}
      {/* Preview toggle handle */}
      <button
        onClick={() => setPreviewCollapsed(v => !v)}
        title={previewCollapsed ? t('show_preview') : t('hide_preview')}
        className="w-5 shrink-0 self-stretch flex items-center justify-center text-gray-400 dark:text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800 hover:text-gray-600 dark:hover:text-gray-300 transition-colors border-l border-gray-200 dark:border-gray-800"
      >
        {previewCollapsed ? '◀' : '▶'}
      </button>

      {contextMenu && (
        <div
          className="fixed z-50 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-lg py-1 min-w-[160px]"
          style={{
            left: Math.min(contextMenu.x, Math.max(0, window.innerWidth - 190)),
            top: Math.min(contextMenu.y, Math.max(0, window.innerHeight - 200)),
          }}
        >
          {selectedIds.size <= 1 ? (
            <>
              <button
                className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
                onClick={() => { navigator.clipboard.writeText(contextMenu.item.rel_path).catch(e => console.warn('复制失败:', e)); setContextMenu(null) }}
              >
                {t('copy_path')}
              </button>
              <button
                className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
                onClick={() => { openFile(contextMenu.item.file_id); setContextMenu(null) }}
              >
                {t('open')}
              </button>
              <button
                className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
                onClick={() => { revealInFolder(contextMenu.item.file_id); setContextMenu(null) }}
              >
                {t('show_in_folder')}
              </button>
              <button
                className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
                onClick={async () => {
                  const fid = contextMenu.item.file_id
                  setContextMenu(null)
                  try {
                    const text = await previewFile(fid)
                    if (!text.trim()) { alert(t('no_text_to_export')); return }
                    const name = contextMenu.item.file_name.replace(/\.[^.]+$/, '') + '.txt'
                    const path = await save({ defaultPath: name, filters: [{ name: 'Text', extensions: ['txt'] }] })
                    if (path) await writeTextFile(path, text)
                  } catch (e) { console.error('[Browse] export text failed:', e) }
                }}
              >
                {t('export_text')}
              </button>
            </>
          ) : null}
          <button
            className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
            onClick={async () => {
              const ids = selectedIds.size > 1 ? [...selectedIds] : [contextMenu.item.file_id]
              // 重索引会覆盖已索引文件的现有数据：全部已索引时才需确认；未索引的直接重建。
              const targets = items.filter(i => ids.includes(i.file_id))
              if (targets.length === ids.length && targets.every(i => i.indexed === 1)) {
                const ok = await ask(t('confirm_reindex'), { title: t('reindex'), kind: 'warning' })
                if (!ok) { setContextMenu(null); return }
              }
              reindexFiles(ids).catch(e => console.error('[Browse] reindex failed:', e))
              setContextMenu(null); setSelectedIds(new Set()); loadFiles()
            }}
          >
            {selectedIds.size > 1 ? t('batch_reindex', { n: selectedIds.size }) : t('reindex')}
          </button>
          {selectedIds.size <= 1 ? (
            <button
              className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
              onClick={() => { viewIndexLog(contextMenu.item.file_id); setContextMenu(null) }}
            >
              {t('view_index_log')}
            </button>
          ) : null}
        </div>
      )}

      {indexLog !== null && (
        <div className="fixed inset-0 z-50 bg-black/30 flex items-center justify-center" onClick={() => setIndexLog(null)}>
          <div className="bg-white dark:bg-gray-900 rounded-lg shadow-xl max-w-2xl w-full mx-4 max-h-[70vh] overflow-auto" onClick={e => e.stopPropagation()}>
            <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-800 flex items-center justify-between">
              <span className="text-sm font-medium">{t('index_log_title')}</span>
              <button onClick={() => setIndexLog(null)} className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-200">×</button>
            </div>
            <pre className="p-4 text-xs text-gray-700 dark:text-gray-300 whitespace-pre-wrap font-mono leading-relaxed">
              {indexLogLoading ? t('loading') : indexLog}
            </pre>
          </div>
        </div>
      )}
    </div>
  )
}
