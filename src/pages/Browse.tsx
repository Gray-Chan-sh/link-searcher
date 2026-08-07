import { useEffect, useState, useCallback, useRef } from 'react'
import { useSearchParams } from 'react-router-dom'
import { invoke } from '@tauri-apps/api/core'
import { convertFileSrc } from '@tauri-apps/api/core'
import { ask } from '@tauri-apps/plugin-dialog'
import { useI18n } from '../i18n'
import { type FilePreview, openFile, revealInFolder } from '../api/files'
import { type FileItem, type FilterType, type SortKey, type SortOrder, listFilesDb, getBrowseFileTypes } from '../api/files'
import { reindexFile } from '../api/index'
import { LoadingSpinner } from '../icons'

function statusBadge(indexed: number, error_msg: string | null | undefined, t: (k: string) => string) {
  if (indexed === 1 || indexed === 3) return <span className="inline-flex items-center gap-1 text-xs text-green-600 dark:text-green-400">✓ {t('indexed')}</span>
  if (indexed === 2) return <span className="inline-flex items-center gap-1 text-xs text-red-600 dark:text-red-400" title={error_msg ?? undefined}>✗ {t('failed')}</span>
  return <span className="inline-flex items-center gap-1 text-xs text-yellow-600 dark:text-yellow-400">○ {t('pending')}</span>
}

export default function Browse() {
  const { t } = useI18n()
  const [params, setParams] = useSearchParams()
  const [items, setItems] = useState<FileItem[]>([])
  const [total, setTotal] = useState(0)
  const [page, setPage] = useState(1)
  const [pageSize] = useState(20)
  const [filter, setFilter] = useState<FilterType>(params.get('filter') as FilterType || 'all')
  const [ext, setExt] = useState(params.get('ext') || '')
  const [availableExts, setAvailableExts] = useState<string[]>([])
  const [search, setSearch] = useState(params.get('search') || '')
  const [debouncedSearch, setDebouncedSearch] = useState(search)
  const [sort, setSort] = useState<SortKey>(params.get('sort') as SortKey || 'name')
  const [order, setOrder] = useState<SortOrder>(params.get('order') as SortOrder || 'asc')
  const [loading, setLoading] = useState(false)
  const [selectedFile, setSelectedFile] = useState<string | null>(null)
  const [preview, setPreview] = useState<FilePreview | null>(null)
  const [previewLoading, setPreviewLoading] = useState(false)
  const [previewError, setPreviewError] = useState<string | null>(null)
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; item: FileItem } | null>(null)
  const [indexLog, setIndexLog] = useState<string | null>(null)
  const [indexLogLoading, setIndexLogLoading] = useState(false)
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set())
  const [lastClickedIdx, setLastClickedIdx] = useState<number | null>(null)
  const [colWidths, setColWidths] = useState({ filename: 192, path: 200, type: 64, status: 112 })
  type ColKey = keyof typeof colWidths
  const resizingRef = useRef<{ col: ColKey; startX: number; startWidth: number } | null>(null)
  const tableRef = useRef<HTMLDivElement>(null)

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
    const colIdx = col === 'filename' ? 0 : 1
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
      const res = await listFilesDb({ filter, ext: ext || undefined, search: debouncedSearch || undefined, sort, order, page, pageSize })
      setItems(res.items)
      setTotal(res.total)
    } catch {
      setItems([])
      setTotal(0)
    } finally {
      setLoading(false)
    }
  }, [filter, ext, debouncedSearch, sort, order, page, pageSize])

  // Clamp page to a valid range when the result set shrinks (e.g. after a
  // re-scan fixes failures), so the user is never left on an empty page.
  const totalPages = Math.max(1, Math.ceil(total / pageSize))
  useEffect(() => {
    if (page > totalPages) setPage(totalPages)
  }, [page, totalPages])

  const handleReindex = useCallback(async (item: FileItem) => {
    if (item.indexed === 1) {
      const confirmed = await ask(t('confirm_reindex'), { title: t('reindex'), kind: 'warning' })
      if (!confirmed) return
    }
    reindexFile(item.file_id).catch(() => {}).finally(() => loadFiles())
  }, [t, loadFiles])

  const viewIndexLog = useCallback(async (fileId: string) => {
    setIndexLogLoading(true)
    setIndexLog(null)
    try {
      const lines: string[] = await invoke('get_logs', { lines: 500 })
      const filtered = lines.filter(l => l.includes(`[${fileId}]`))
      setIndexLog(filtered.join('\n') || '未找到此文件的索引日志')
    } catch {
      setIndexLog('获取日志失败')
    } finally {
      setIndexLogLoading(false)
    }
  }, [])

  useEffect(() => {
    const t = setTimeout(() => setDebouncedSearch(search), 300)
    return () => clearTimeout(t)
  }, [search])

  // Cmd/Ctrl+A: select all
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'a' && items.length > 0) {
        e.preventDefault()
        setSelectedIds(new Set(items.map(i => i.file_id)))
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [items])

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

  return (
    <div className="flex h-full">
      {/* Left: Table */}
      <div className="flex-1 flex flex-col min-w-0 border-r border-gray-200 dark:border-gray-800">
        {/* Toolbar */}
        <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-800 flex items-center gap-3 flex-wrap">
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
              onChange={e => { setSearch(e.target.value); setPage(1) }}
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
        </div>

        {/* Table */}
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
                  <th className="px-2 py-1 font-medium">{t('type')}</th>
                  <th className="px-2 py-1 font-medium">{t('status')}</th>
                </tr>
              </thead>
              <tbody>
                {items.map((item, idx) => (
                  <tr
                    key={item.file_id}
                    onClick={(e) => {
                      if (e.metaKey || e.ctrlKey) {
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
                        setSelectedIds(new Set())
                        setLastClickedIdx(idx)
                      }
                    }}
                    onContextMenu={(e) => { e.preventDefault(); setContextMenu({ x: e.clientX, y: e.clientY, item }) }}
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
      </div>

      {/* Right: Preview */}
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
            {preview.file_type === 'image' && preview.image_path && (
              <div className="mb-3 flex justify-center">
                <img
                  src={convertFileSrc(preview.image_path)}
                  alt=""
                  className="max-w-full max-h-64 object-contain rounded-lg border border-gray-200 dark:border-gray-700"
                />
              </div>
            )}
            {preview.content && (
              <pre className="text-xs text-gray-700 dark:text-gray-300 whitespace-pre-wrap font-mono leading-relaxed max-h-96 overflow-y-auto">
                {preview.content}
              </pre>
            )}
            {!preview.content && preview.file_type !== 'image' && (
              <p className="text-sm text-gray-400">{t('no_preview_available')}</p>
            )}
            {preview.char_count > 0 && (
              <p className="mt-3 text-xs text-gray-400 border-t border-gray-200 dark:border-gray-800 pt-2">
                {t('characters_count', { count: preview.char_count })} {preview.ocr_used ? '(OCR)' : ''}
              </p>
            )}
          </div>
        )}
        {!preview && !previewLoading && !previewError && selectedFile && (
          <p className="p-4 text-sm text-gray-400">{t('loading_preview')}</p>
        )}
        {!selectedFile && !previewLoading && (
          <div className="flex items-center justify-center h-full text-sm text-gray-400">
            {t('select_file_preview')}
          </div>
        )}
      </div>

      {contextMenu && (
        <div
          className="fixed z-50 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-lg py-1 min-w-[160px]"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          {selectedIds.size <= 1 && (<>
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
          </>)}
          <button
            className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
            onClick={() => {
              const ids = selectedIds.size > 1 ? [...selectedIds] : [contextMenu.item.file_id]
              ids.forEach(id => reindexFile(id).catch(() => {}))
              setContextMenu(null); setSelectedIds(new Set()); loadFiles()
            }}
          >
            {selectedIds.size > 1 ? `批量重新索引 (${selectedIds.size})` : t('reindex')}
          </button>
          <button
            className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
            onClick={() => { viewIndexLog(contextMenu.item.file_id); setContextMenu(null) }}
          >
            查看索引日志
          </button>
        </div>
      )}

      {indexLog !== null && (
        <div className="fixed inset-0 z-50 bg-black/30 flex items-center justify-center" onClick={() => setIndexLog(null)}>
          <div className="bg-white dark:bg-gray-900 rounded-lg shadow-xl max-w-2xl w-full mx-4 max-h-[70vh] overflow-auto" onClick={e => e.stopPropagation()}>
            <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-800 flex items-center justify-between">
              <span className="text-sm font-medium">索引日志</span>
              <button onClick={() => setIndexLog(null)} className="text-gray-400 hover:text-gray-600">×</button>
            </div>
            <pre className="p-4 text-xs text-gray-700 dark:text-gray-300 whitespace-pre-wrap font-mono leading-relaxed">
              {indexLogLoading ? '加载中...' : indexLog}
            </pre>
          </div>
        </div>
      )}
    </div>
  )
}
