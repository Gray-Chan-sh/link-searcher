import { useEffect, useState, useCallback, useRef } from 'react'
import { resolveAssetUrlSync as resolveAssetUrl } from '../utils/platform'
import { getFilePreview, openFile, revealInFolder, type FilePreview } from '../api/files'
import { type FileItem, listFilesDb } from '../api/files'
import { reExtractFile } from '../api/index'
import { useI18n } from '../i18n'
import { LoadingSpinner } from '../icons'
import { toast } from '../utils/toast'

function parseQualityFlags(raw: string): string[] {
  try {
    const parsed: unknown = JSON.parse(raw)
    if (!Array.isArray(parsed)) return []
    return parsed.filter((f): f is string => typeof f === 'string')
  } catch {
    return []
  }
}

function qualityDotClass(score: number | null): string {
  if (score == null) return 'bg-gray-300 dark:bg-gray-600'
  if (score > 0.75) return 'bg-green-500'
  if (score >= 0.5) return 'bg-yellow-500'
  return 'bg-red-500'
}

const IMAGE_EXTS = new Set(['.png', '.jpg', '.jpeg', '.gif', '.bmp', '.webp', '.tiff', '.tif', '.heic', '.heif'])

function formatFileSize(bytes: number): string {
  if (bytes === 0) return '0 B'
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

interface ProblemCategory {
  labelKey: string
  reasonKey: string
  solutionKey: string
  badgeColor: string
}

function deriveProblemCategory(
  fileSize: number,
  charCount: number,
  fileExt: string,
  indexed: number,
  _errorMsg: string | null,
): ProblemCategory | null {
  if (fileSize === 0) {
    return {
      labelKey: 'quality_cat_empty_file',
      reasonKey: 'quality_cat_empty_file_reason',
      solutionKey: 'quality_cat_empty_file_solution',
      badgeColor: 'bg-gray-100 dark:bg-gray-800 text-gray-700 dark:text-gray-300 border-gray-200 dark:border-gray-700',
    }
  }
  if (indexed === 2) {
    return {
      labelKey: 'quality_cat_index_failed',
      reasonKey: 'quality_cat_index_failed_reason',
      solutionKey: 'quality_cat_index_failed_solution',
      badgeColor: 'bg-red-50 dark:bg-red-900/20 text-red-700 dark:text-red-300 border-red-200 dark:border-red-800/40',
    }
  }
  if (charCount === 0 && fileSize > 0) {
    const ext = fileExt.startsWith('.') ? fileExt.toLowerCase() : `.${fileExt}`.toLowerCase()
    if (IMAGE_EXTS.has(ext)) {
      return {
        labelKey: 'quality_cat_image_no_text',
        reasonKey: 'quality_cat_image_no_text_reason',
        solutionKey: 'quality_cat_image_no_text_solution',
        badgeColor: 'bg-amber-50 dark:bg-amber-900/20 text-amber-700 dark:text-amber-300 border-amber-200 dark:border-amber-800/40',
      }
    }
    return {
      labelKey: 'quality_cat_no_extracted_text',
      reasonKey: 'quality_cat_no_extracted_text_reason',
      solutionKey: 'quality_cat_no_extracted_text_solution',
      badgeColor: 'bg-orange-50 dark:bg-orange-900/20 text-orange-700 dark:text-orange-300 border-orange-200 dark:border-orange-800/40',
    }
  }
  return null
}

export default function Quality() {
  const { t } = useI18n()
  const [items, setItems] = useState<FileItem[]>([])
  const [total, setTotal] = useState(0)
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(20)
  const [showAll, setShowAll] = useState(false)
  const [search, setSearch] = useState('')
  const [loading, setLoading] = useState(false)
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [preview, setPreview] = useState<FilePreview | null>(null)
  const [previewLoading, setPreviewLoading] = useState(false)
  const [previewError, setPreviewError] = useState<string | null>(null)
  const [reExtracting, setReExtracting] = useState(false)
  const tableRef = useRef<HTMLDivElement>(null)
  const rowHeightRef = useRef<number | null>(null)
  const previewVersionRef = useRef(0)
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

  useEffect(() => {
    const el = tableRef.current
    if (!el) return
    const row = el.querySelector<HTMLTableRowElement>('tbody tr')
    if (row) {
      rowHeightRef.current = row.getBoundingClientRect().height
      setPageSize(Math.min(1000, Math.max(1, Math.floor(el.clientHeight / rowHeightRef.current))))
    }
  }, [items])

  const loadFiles = useCallback(async () => {
    setLoading(true)
    try {
      const qualityParam = showAll ? undefined : 'flagged'
      const res = await listFilesDb({
        quality: qualityParam,
        sort: 'quality',
        order: 'asc',
        page,
        pageSize,
        search: search || undefined,
      })
      setItems(res.items)
      setTotal(res.total)
    } catch {
      setItems([])
      setTotal(0)
    } finally {
      setLoading(false)
    }
  }, [showAll, search, page, pageSize])

  const totalPages = Math.max(1, Math.ceil(total / pageSize))
  useEffect(() => {
    if (page > totalPages) setPage(totalPages)
  }, [page, totalPages])

  useEffect(() => {
    loadFiles()
  }, [loadFiles])

  const selectFile = useCallback(async (fileId: string) => {
    const localVersion = ++previewVersionRef.current
    setSelectedId(fileId)
    setPreviewLoading(true)
    setPreviewError(null)
    setPreview(null)
    try {
      const result = await getFilePreview(fileId)
      if (previewVersionRef.current !== localVersion) return
      setPreview(result)
    } catch (e) {
      if (previewVersionRef.current !== localVersion) return
      setPreviewError(e instanceof Error ? e.message : t('failed_load_preview'))
    }
    if (previewVersionRef.current !== localVersion) return
    setPreviewLoading(false)
  }, [t])

  const handleReExtract = useCallback(async () => {
    if (!selectedId || reExtracting) return
    setReExtracting(true)
    try {
      await reExtractFile(selectedId)
      toast(t('quality_reextract_done'))
    } catch (e) {
      toast(e instanceof Error ? e.message : t('quality_reextract_failed'), 'error')
    }
    await loadFiles()
    if (selectedId) {
      try {
        const p = await getFilePreview(selectedId)
        setPreview(p)
      } catch {
        // preview may have become invalid if file re-indexed
      }
    }
    setReExtracting(false)
  }, [selectedId, reExtracting, loadFiles, t])

  const flagReasonKey = (flag: string): string =>
    `quality_flag_${flag}_reason` as const
  const flagSolutionKey = (flag: string): string =>
    `quality_flag_${flag}_solution` as const

  const isImage = preview?.file_type === 'image'
  const imageSrc = isImage && preview.image_path
    ? (preview.image_base64
        ? (() => {
            const ext = (preview.image_path!.split('.').pop() || 'jpeg').toLowerCase()
            const mime = ({ jpg: 'jpeg', jpeg: 'jpeg', png: 'png', gif: 'gif', webp: 'webp', bmp: 'bmp', tiff: 'tiff', tif: 'tiff' } as Record<string, string>)[ext] || 'jpeg'
            return `data:image/${mime};base64,${preview.image_base64}`
          })()
        : resolveAssetUrl(preview.image_path))
    : null

  return (
    <div className="flex h-full">
      <div className="flex-1 flex flex-col min-w-0 border-r border-gray-200 dark:border-gray-800">
        <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-800 flex items-center gap-3 flex-wrap">
          <span className="text-sm font-semibold text-gray-900 dark:text-gray-100 shrink-0">{t('quality_page_title')}</span>
          <div className="flex items-center bg-gray-100 dark:bg-gray-800 rounded-lg p-0.5 shrink-0">
            <button
              onClick={() => { setShowAll(false); setPage(1) }}
              className={`px-3 py-1 text-xs font-medium rounded-md transition-colors ${
                !showAll
                  ? 'bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 shadow-sm'
                  : 'text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300'
              }`}
            >
              {t('quality_show_flagged')}
            </button>
            <button
              onClick={() => { setShowAll(true); setPage(1) }}
              className={`px-3 py-1 text-xs font-medium rounded-md transition-colors ${
                showAll
                  ? 'bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 shadow-sm'
                  : 'text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300'
              }`}
            >
              {t('quality_show_all')}
            </button>
          </div>
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
              <button onClick={() => { setSearch(''); setPage(1) }} className="absolute right-1.5 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">×</button>
            )}
          </div>
          <button onClick={() => loadFiles()} disabled={loading} className="text-xs px-2 py-1 text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 disabled:opacity-50" title={t('refresh')}>↻</button>
          <span className="text-xs text-gray-400 dark:text-gray-500 ml-auto">{total.toLocaleString()} {t('files')}</span>
        </div>

        <div className="flex-1 overflow-auto" ref={tableRef}>
          {loading ? (
            <div className="flex items-center justify-center py-16">
              <LoadingSpinner className="size-5" />
            </div>
          ) : items.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-16 px-4 text-center space-y-2">
              <p className="text-sm font-medium text-gray-900 dark:text-gray-100">
                {showAll ? t('quality_no_files') : t('quality_no_flagged_files')}
              </p>
              <p className="text-xs text-gray-500 dark:text-gray-400">
                {showAll ? t('quality_no_files_hint') : t('quality_no_flagged_hint')}
              </p>
            </div>
          ) : (
            <table className="w-full text-xs select-none table-fixed">
              <thead className="sticky top-0 bg-gray-50 dark:bg-gray-900/80 backdrop-blur z-10">
                <tr className="border-b border-gray-200 dark:border-gray-800 text-gray-500 dark:text-gray-400 text-left">
                  <th className="px-2 py-1 font-medium" style={{ width: 220 }}>{t('filename')}</th>
                  <th className="px-2 py-1 font-medium" style={{ width: 100 }}>{t('path')}</th>
                  <th className="px-2 py-1 font-medium" style={{ width: 60 }}>{t('quality_source_size')}</th>
                  <th className="px-2 py-1 font-medium" style={{ width: 60 }}>{t('quality_char_count_label')}</th>
                  <th className="px-2 py-1 font-medium" style={{ width: 70 }}>{t('quality')}</th>
                  <th className="px-2 py-1 font-medium" style={{ width: 70 }}>{t('quality_flags_label')}</th>
                </tr>
              </thead>
              <tbody>
                {items.map(item => {
                  const flags = parseQualityFlags(item.quality_flags)
                  const isSelected = selectedId === item.file_id
                  return (
                    <tr
                      key={item.file_id}
                      onClick={() => selectFile(item.file_id)}
                      className={`border-b border-gray-100 dark:border-gray-800/50 cursor-pointer transition-colors ${
                        isSelected ? 'bg-blue-50 dark:bg-blue-900/20' : 'hover:bg-gray-50 dark:hover:bg-gray-800/50'
                      }`}
                    >
                      <td className="px-2 py-1">
                        <div className="flex items-center gap-2">
                          <span className={`size-2 rounded-full shrink-0 ${qualityDotClass(item.quality_score)}`} />
                          <span className="truncate" title={item.file_name}>{item.file_name}</span>
                        </div>
                      </td>
                      <td className="px-2 py-1">
                        <span className="text-gray-500 dark:text-gray-400 truncate block" title={item.rel_path}>{item.rel_path}</span>
                      </td>
                      <td className="px-2 py-1">
                        <span className="text-gray-500 dark:text-gray-400 tabular-nums">{formatFileSize(item.file_size)}</span>
                      </td>
                      <td className="px-2 py-1">
                        <span className="text-gray-500 dark:text-gray-400 tabular-nums">{item.char_count.toLocaleString()}</span>
                      </td>
                      <td className="px-2 py-1">
                        <span className={`font-medium ${
                          item.quality_score == null ? 'text-gray-400 dark:text-gray-500' :
                          item.quality_score > 0.75 ? 'text-green-600 dark:text-green-400' :
                          item.quality_score >= 0.5 ? 'text-yellow-600 dark:text-yellow-400' :
                          'text-red-600 dark:text-red-400'
                        }`}>
                          {item.quality_score != null ? `${(item.quality_score * 100).toFixed(0)}%` : '—'}
                        </span>
                      </td>
                      <td className="px-2 py-1">
                        {flags.length > 0 && (
                          <span className="px-1.5 py-0.5 text-[10px] font-medium text-amber-700 dark:text-amber-300 bg-amber-50 dark:bg-amber-900/30 border border-amber-200 dark:border-amber-800/40 rounded">
                            {flags.length}
                          </span>
                        )}
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          )}
        </div>

        {totalPages > 1 && (
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
                onChange={e => {
                  const p = parseInt(e.target.value, 10)
                  if (!isNaN(p) && p >= 1 && p <= totalPages) setPage(p)
                }}
                onKeyDown={e => {
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
        )}
      </div>

      <div className="w-96 shrink-0 overflow-y-auto bg-white dark:bg-gray-900 flex flex-col">
        {previewLoading && (
          <div className="flex items-center justify-center py-16">
            <LoadingSpinner className="size-5" />
          </div>
        )}
        {previewError && (
          <div className="p-4 text-sm text-red-600 dark:text-red-400">{previewError}</div>
        )}
        {!selectedId && !previewLoading && (
          <div className="flex items-center justify-center h-full text-sm text-gray-400 dark:text-gray-500">
            {t('quality_select_file')}
          </div>
        )}
        {preview && !previewLoading && !previewError && (
            <div className="p-4 space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">
                {items.find(i => i.file_id === selectedId)?.file_name ?? ''}
              </h3>
            </div>
            {(() => {
              const item = items.find(i => i.file_id === selectedId)
              if (!item) return null
              return (
                <div className="flex items-center gap-3 text-[10px] text-gray-400 dark:text-gray-500">
                  <span>{t('quality_source_size')}: {formatFileSize(item.file_size)}</span>
                  <span>·</span>
                  <span>{t('quality_char_count_label')}: {item.char_count.toLocaleString()}</span>
                </div>
              )
            })()}

            <div className="space-y-2 border-b border-gray-100 dark:border-gray-800 pb-4">
              <span className="text-xs font-medium text-gray-500 dark:text-gray-400">{t('quality_original_preview')}</span>
              {isImage && imageSrc ? (
                <div className="flex justify-center">
                  <img src={imageSrc} alt="" className="max-w-full max-h-64 object-contain rounded border border-gray-200 dark:border-gray-700" />
                </div>
              ) : (
                <div className="flex gap-2">
                  <button
                    onClick={() => { if (selectedId) openFile(selectedId) }}
                    className="px-3 py-1.5 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 border border-blue-200 dark:border-blue-800/40 rounded-md hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
                  >
                    {t('quality_open_original')}
                  </button>
                  <button
                    onClick={() => { if (selectedId) revealInFolder(selectedId) }}
                    className="px-3 py-1.5 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                  >
                    {t('show_in_folder')}
                  </button>
                </div>
              )}
            </div>

            {preview.content && (
              <div className="space-y-2 border-b border-gray-100 dark:border-gray-800 pb-4">
                <div className="flex items-center gap-2">
                  <span className="text-xs font-medium text-gray-500 dark:text-gray-400">{t('quality_text_preview')}</span>
                  <span className="text-[10px] text-gray-400 dark:text-gray-500">{t('quality_char_count', { count: preview.char_count })}</span>
                  {preview.ocr_used && (
                    <span className="px-1.5 py-0.5 text-[10px] font-medium text-amber-700 dark:text-amber-300 bg-amber-50 dark:bg-amber-900/30 border border-amber-200 dark:border-amber-800/40 rounded">
                      {t('quality_ocr_badge')}
                    </span>
                  )}
                </div>
                <pre className="text-xs text-gray-700 dark:text-gray-300 whitespace-pre-wrap font-mono leading-relaxed bg-gray-50 dark:bg-gray-800/50 rounded border border-gray-200 dark:border-gray-700 p-3 max-h-64 overflow-y-auto">
                  {preview.content.length > 50000 ? preview.content.slice(0, 50000) + '…' : preview.content}
                </pre>
                {preview.content.length > 50000 && (
                  <p className="text-xs text-amber-600 dark:text-amber-400">{t('truncated_notice')}</p>
                )}
              </div>
            )}

            {(() => {
              const flags = parseQualityFlags(preview.quality_flags)
              const item = items.find(i => i.file_id === selectedId)
              const category = item
                ? deriveProblemCategory(item.file_size, item.char_count, item.file_ext, item.indexed, item.error_msg)
                : null

              if (!category && flags.length === 0) return null

              return (
                <div className="space-y-3">
                  {category && (
                    <div className={`border rounded-md px-3 py-2 ${category.badgeColor}`}>
                      <div className="text-xs font-semibold">{t(category.labelKey)}</div>
                      <div className="text-xs mt-1 opacity-80">{t(category.reasonKey)}</div>
                      <div className="text-xs mt-1 opacity-80">{t(category.solutionKey)}</div>
                    </div>
                  )}

                  {flags.length > 0 && (
                    <>
                      <div>
                        <h4 className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-2">{t('quality_why_low')}</h4>
                        <ul className="space-y-1.5">
                          {flags.map(f => (
                            <li key={f} className="text-xs text-red-700 dark:text-red-300 bg-red-50 dark:bg-red-900/10 border border-red-100 dark:border-red-800/30 rounded px-2.5 py-1.5">
                              <span className="font-medium">{t(flagReasonKey(f))}</span>
                            </li>
                          ))}
                        </ul>
                      </div>
                      <div>
                        <h4 className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-2">{t('quality_recommended_fix')}</h4>
                        <ul className="space-y-1.5">
                          {flags.map(f => (
                            <li key={f} className="text-xs text-blue-700 dark:text-blue-300 bg-blue-50 dark:bg-blue-900/10 border border-blue-100 dark:border-blue-800/30 rounded px-2.5 py-1.5">
                              <span className="font-medium">{t(flagSolutionKey(f))}</span>
                            </li>
                          ))}
                        </ul>
                      </div>
                    </>
                  )}
                </div>
              )
            })()}

            <button
              onClick={handleReExtract}
              disabled={reExtracting}
              className="w-full px-3 py-2 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800/40 rounded-md hover:bg-blue-100 dark:hover:bg-blue-900/40 transition-colors disabled:opacity-40 disabled:cursor-not-allowed flex items-center justify-center gap-1.5"
            >
              {reExtracting && <LoadingSpinner className="size-3.5" />}
              {reExtracting ? t('quality_re_extracting') : t('quality_re_extract')}
            </button>
          </div>
        )}
      </div>
    </div>
  )
}
