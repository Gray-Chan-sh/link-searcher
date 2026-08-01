import { useEffect, useState, useCallback, useRef } from 'react'
import { useSearchParams } from 'react-router-dom'
import { invoke } from '@tauri-apps/api/core'
import { convertFileSrc } from '@tauri-apps/api/core'
import { type FilePreview } from '../api/files'
import { type FileItem, type FilterType, type SortKey, type SortOrder, listFilesDb } from '../api/files'
import { LoadingSpinner } from '../icons'

function statusBadge(indexed: number, error_msg: string | null | undefined) {
  if (indexed === 1) return <span className="inline-flex items-center gap-1 text-xs text-green-600 dark:text-green-400">✓ Indexed</span>
  if (indexed === 2) return <span className="inline-flex items-center gap-1 text-xs text-red-600 dark:text-red-400" title={error_msg ?? undefined}>✗ Failed</span>
  return <span className="inline-flex items-center gap-1 text-xs text-yellow-600 dark:text-yellow-400">○ Pending</span>
}

export default function Browse() {
  const [params, setParams] = useSearchParams()
  const [items, setItems] = useState<FileItem[]>([])
  const [total, setTotal] = useState(0)
  const [page, setPage] = useState(1)
  const [pageSize] = useState(20)
  const [filter, setFilter] = useState<FilterType>(params.get('filter') as FilterType || 'all')
  const [ext, setExt] = useState(params.get('ext') || '')
  const [search, setSearch] = useState(params.get('search') || '')
  const [debouncedSearch, setDebouncedSearch] = useState(search)
  const [sort, setSort] = useState<SortKey>(params.get('sort') as SortKey || 'name')
  const [order, setOrder] = useState<SortOrder>(params.get('order') as SortOrder || 'asc')
  const [loading, setLoading] = useState(false)
  const [selectedFile, setSelectedFile] = useState<string | null>(null)
  const [preview, setPreview] = useState<FilePreview | null>(null)
  const [previewLoading, setPreviewLoading] = useState(false)
  const [previewError, setPreviewError] = useState<string | null>(null)

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

  const loadFiles = useCallback(async () => {
    setLoading(true)
    try {
      const res = await listFilesDb({ filter, ext: ext || undefined, search: debouncedSearch || undefined, sort, order, page, page_size: pageSize })
      setItems(res.items)
      setTotal(res.total)
    } catch {
      setItems([])
      setTotal(0)
    } finally {
      setLoading(false)
    }
  }, [filter, ext, debouncedSearch, sort, order, page, pageSize])

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
      setPreviewError(typeof e === 'string' ? e : 'Failed to load preview')
    }
    if (previewVersionRef.current !== localVersion) return
    setPreviewLoading(false)
  }, [])

  const totalPages = Math.max(1, Math.ceil(total / pageSize))

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
            <option value="all">All</option>
            <option value="indexed">Indexed</option>
            <option value="pending">Pending</option>
            <option value="failed">Failed</option>
          </select>

          <select
            value={ext}
            onChange={e => { setExt(e.target.value); setPage(1) }}
            className="text-xs bg-transparent border border-gray-200 dark:border-gray-700 rounded px-2 py-1 text-gray-600 dark:text-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500"
          >
            <option value="">All Types</option>
            <option value="pdf">PDF</option>
            <option value="docx">DOCX</option>
            <option value="doc">DOC</option>
            <option value="xlsx">XLSX</option>
            <option value="txt">TXT</option>
            <option value="md">MD</option>
            <option value="png">PNG</option>
            <option value="jpg">JPG</option>
            <option value="jpeg">JPEG</option>
            <option value="pptx">PPTX</option>
            <option value="csv">CSV</option>
          </select>

          <div className="relative flex-1 min-w-[160px] max-w-xs">
            <input
              type="text"
              value={search}
              onChange={e => { setSearch(e.target.value); setPage(1) }}
              onKeyDown={e => e.key === 'Enter' && setPage(1)}
              placeholder="Search filename..."
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
            <option value="name-asc">Name A-Z</option>
            <option value="name-desc">Name Z-A</option>
            <option value="size-desc">Size ↓</option>
            <option value="size-asc">Size ↑</option>
            <option value="mtime-desc">Newest</option>
            <option value="mtime-asc">Oldest</option>
            <option value="ext-asc">Ext A-Z</option>
          </select>

          <span className="text-xs text-gray-400 dark:text-gray-500 ml-auto">{total.toLocaleString()} files</span>
        </div>

        {/* Table */}
        <div className="flex-1 overflow-y-auto">
          {loading ? (
            <div className="flex items-center justify-center py-16">
              <LoadingSpinner className="size-5" />
            </div>
          ) : (
            <table className="w-full text-xs">
              <thead className="sticky top-0 bg-gray-50 dark:bg-gray-900/80 backdrop-blur z-10">
                <tr className="border-b border-gray-200 dark:border-gray-800 text-gray-500 dark:text-gray-400 text-left">
                  <th className="px-4 py-2 font-medium w-48">Filename</th>
                  <th className="px-4 py-2 font-medium">Path</th>
                  <th className="px-4 py-2 font-medium w-16">Type</th>
                  <th className="px-4 py-2 font-medium w-28">Status</th>
                </tr>
              </thead>
              <tbody>
                {items.map(item => (
                  <tr
                    key={item.file_id}
                    onClick={() => selectFile(item.rel_path)}
                    className={`border-b border-gray-100 dark:border-gray-800/50 cursor-pointer transition-colors ${
                      selectedFile === item.rel_path
                        ? 'bg-blue-50 dark:bg-blue-900/20'
                        : 'hover:bg-gray-50 dark:hover:bg-gray-800/50'
                    }`}
                  >
                    <td className="px-4 py-2">
                      <div className="flex items-center gap-2">
                        <span className={`size-2 rounded-full shrink-0 ${
                          item.indexed === 1 ? 'bg-green-500' : item.indexed === 2 ? 'bg-red-500' : 'bg-yellow-500'
                        }`} />
                        <span className="truncate max-w-[180px]" title={item.file_name}>{item.file_name}</span>
                      </div>
                    </td>
                    <td className="px-4 py-2">
                      <span className="text-gray-500 dark:text-gray-400 truncate block max-w-[280px]" title={item.rel_path}>{item.rel_path}</span>
                    </td>
                    <td className="px-4 py-2">
                      <span className="text-gray-500 dark:text-gray-400 uppercase">{item.file_ext || '—'}</span>
                    </td>
                    <td className="px-4 py-2">{statusBadge(item.indexed, item.error_msg)}</td>
                  </tr>
                ))}
                {!loading && items.length === 0 && (
                  <tr>
                    <td colSpan={4} className="px-4 py-16 text-center text-gray-400 dark:text-gray-500">
                      No files found
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
            ← Prev
          </button>
          <span className="text-xs text-gray-500 dark:text-gray-400">
            Page {page} / {totalPages} · {((page - 1) * pageSize) + 1}-{Math.min(page * pageSize, total)} of {total}
          </span>
          <button
            onClick={() => setPage(p => Math.min(totalPages, p + 1))}
            disabled={page >= totalPages}
            className="px-3 py-1 text-xs border border-gray-200 dark:border-gray-700 rounded hover:bg-gray-100 dark:hover:bg-gray-800 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
          >
            Next →
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
          <div className="p-4">
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
              <p className="text-sm text-gray-400">No preview available</p>
            )}
            {preview.char_count > 0 && (
              <p className="mt-3 text-xs text-gray-400 border-t border-gray-200 dark:border-gray-800 pt-2">
                {preview.char_count} characters {preview.ocr_used ? '(OCR)' : ''}
              </p>
            )}
          </div>
        )}
        {!preview && !previewLoading && !previewError && selectedFile && (
          <p className="p-4 text-sm text-gray-400">Loading preview...</p>
        )}
        {!selectedFile && !previewLoading && (
          <div className="flex items-center justify-center h-full text-sm text-gray-400">
            Select a file to preview
          </div>
        )}
      </div>
    </div>
  )
}
