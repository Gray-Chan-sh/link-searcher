import { useEffect, useState, useRef, useMemo, useCallback } from 'react'
import { createPortal } from 'react-dom'
import { resolveAssetUrlSync as resolveAssetUrl, exportFile } from '../utils/platform'
import { getFile, getFilePreview, openFile, revealInFolder, summarizeFile, aiCapabilities, exportFileText, type FileDetail, type FilePreview, type SummaryResult, type AiCapabilities, type ExportFormat } from '../api/files'
import { reExtractFile } from '../api/index'
import { invoke } from '../api/client'
import { useI18n } from '../i18n'
import { useIsMobile } from '../hooks/useIsMobile'
import { usePersistentState } from '../hooks/usePersistentState'
import { XIcon, LoadingSpinner } from '../icons'
import { formatSize, formatTime } from '../utils/format'
import { toast } from '../utils/toast'
import {
  DEFAULT_PREVIEW_FONT_SIZE,
  DEFAULT_PREVIEW_WIDTH,
  LS_PREVIEW_FONT_SIZE,
  LS_PREVIEW_WIDTH,
  clampPreviewWidth,
  imageSrc,
  normalizeFontSize,
  stepFontSize,
} from './preview/panelPrefs'

/** 正文最多渲染的字符数：再多就没法高亮/滚动得动，超出部分提示下载全文。 */
const TEXT_LIMIT = 50000

export interface PreviewPanelProps {
  /** 已索引文件走 id（搜索页、浏览页的表格行）—— 元信息/摘要/重提取/打开都可用。 */
  fileId?: string | null
  /**
   * 只有路径时的兜底（浏览页的 `?path=` 引用深链、未索引文件）。
   * 走 `preview_file_by_path`，元信息与需要 file_id 的操作自动降级隐藏。
   */
  path?: string | null
  /** 关键词高亮 + 上下跳转（搜索页传查询词）。 */
  searchQuery?: string
  onClose: () => void
  /** 浏览页：右侧常驻面板 + 折叠条（折叠后仍能点开）。 */
  collapsible?: boolean
}

function highlightText(text: string, query: string): React.ReactNode[] {
  if (!query.trim()) return [text]
  const terms = query.split(/\s+/).filter(t => t.length > 0).slice(0, 20)
  if (terms.length === 0) return [text]
  const termSet = new Set(terms.map(t => t.toLowerCase()))
  // ReDoS risk assessed: terms are escaped + capped at 20, input is user-controlled search box.
  // No untrusted regex injection possible; safe to use dynamic RegExp.
  const escaped = terms.map(t => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
  const regex = new RegExp(`(${escaped.join('|')})`, 'gi')
  const parts = text.split(regex)
  return parts.map((part, i) =>
    part.length > 0 && termSet.has(part.toLowerCase())
      ? <mark key={i} className="bg-yellow-200 dark:bg-yellow-700/50 rounded px-0.5">{part}</mark>
      : part,
  )
}

function countMatches(text: string, query: string): number {
  if (!query.trim()) return 0
  const terms = query.split(/\s+/).filter(t => t.length > 0).slice(0, 20)
  if (terms.length === 0) return 0
  const termSet = new Set(terms.map(t => t.toLowerCase()))
  const lower = text.toLowerCase()
  let count = 0
  for (const term of termSet) {
    let idx = lower.indexOf(term)
    while (idx !== -1) {
      count++
      idx = lower.indexOf(term, idx + 1)
    }
  }
  return count
}

function parseQualityFlags(raw: string): string[] {
  try {
    const parsed: unknown = JSON.parse(raw)
    if (!Array.isArray(parsed)) return []
    return parsed.filter((f): f is string => typeof f === 'string')
  } catch {
    return []
  }
}

function basename(p: string): string {
  return p.split(/[\\/]/).filter(Boolean).pop() ?? p
}

/**
 * 统一的文件预览面板 —— 搜索页与浏览页共用。
 *
 * 尺寸/字号都记忆在 localStorage（见 `preview/panelPrefs.ts`）：左边缘可拖拽改宽、
 * 双击复位；字号 A− / A+ 四档。正文只有一个滚动容器（不再套内层 max-height），
 * 长文档可以直接滚到底。
 */
export default function PreviewPanel({ fileId, path, searchQuery = '', onClose, collapsible = false }: PreviewPanelProps) {
  const { t } = useI18n()
  const isMobile = useIsMobile()
  const [meta, setMeta] = useState<FileDetail | null>(null)
  const [preview, setPreview] = useState<FilePreview | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [width, setWidth] = usePersistentState<number>(LS_PREVIEW_WIDTH, DEFAULT_PREVIEW_WIDTH)
  const [dragWidth, setDragWidth] = useState<number | null>(null)
  const [fontSize, setFontSize] = usePersistentState<number>(LS_PREVIEW_FONT_SIZE, DEFAULT_PREVIEW_FONT_SIZE)
  const [collapsed, setCollapsed] = useState(false)
  const [fullscreen, setFullscreen] = useState(false)
  const [matchIndex, setMatchIndex] = useState(0)
  const [scale, setScale] = useState(1)
  const [summary, setSummary] = useState<SummaryResult | null>(null)
  const [summaryLoading, setSummaryLoading] = useState(false)
  const [summaryError, setSummaryError] = useState<string | null>(null)
  const [aiCap, setAiCap] = useState<AiCapabilities>({ embedding: false, llm: false })
  const [qualityLoading, setQualityLoading] = useState(false)
  const [exportOpen, setExportOpen] = useState(false)
  const [exporting, setExporting] = useState(false)
  const contentRef = useRef<HTMLDivElement>(null)
  /** 拖拽中的宽度（只在放开鼠标时落 localStorage，避免每帧同步写盘） */
  const dragWidthRef = useRef<number | null>(null)

  const hasTarget = Boolean(fileId) || Boolean(path)
  const activeFontSize = normalizeFontSize(fontSize)
  const effectiveWidth = clampPreviewWidth(dragWidth ?? width, window.innerWidth)

  useEffect(() => { aiCapabilities().then(setAiCap).catch(() => {}) }, [])

  // 视口变窄时把已记忆的宽度收回来，避免面板挤掉列表
  useEffect(() => {
    const onResize = () => setWidth(w => clampPreviewWidth(w, window.innerWidth))
    window.addEventListener('resize', onResize)
    return () => window.removeEventListener('resize', onResize)
  }, [setWidth])

  useEffect(() => {
    if (!fullscreen) return
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setFullscreen(false)
    }
    document.addEventListener('keydown', handler)
    return () => document.removeEventListener('keydown', handler)
  }, [fullscreen])

  useEffect(() => {
    if (!hasTarget) {
      setMeta(null)
      setPreview(null)
      setError(null)
      setLoading(false)
      setMatchIndex(0)
      setScale(1)
      setSummary(null)
      setSummaryError(null)
      return
    }

    let cancelled = false
    setLoading(true)
    setError(null)
    setMeta(null)
    setPreview(null)
    setMatchIndex(0)
    setScale(1)
    setSummary(null)
    setSummaryError(null)

    const metaTask = fileId ? getFile(fileId) : Promise.resolve(null)
    const previewTask = fileId
      ? getFilePreview(fileId)
      : invoke<FilePreview>('preview_file_by_path', { path })

    Promise.all([metaTask, previewTask])
      .then(([f, p]) => {
        if (cancelled) return
        setMeta(f)
        setPreview(p)
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : t('failed_load_preview'))
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })

    return () => { cancelled = true }
  }, [fileId, path, hasTarget, t])

  const textContent = preview?.content ?? ''
  const matchCount = useMemo(() => countMatches(textContent, searchQuery), [textContent, searchQuery])
  const truncated = textContent.length > TEXT_LIMIT
  const shownText = truncated ? textContent.slice(0, TEXT_LIMIT) : textContent
  const displayName = meta?.file_name ?? (path ? basename(path) : t('preview'))

  const handleSummarize = useCallback(async () => {
    if (!meta || summaryLoading) return
    setSummaryLoading(true)
    setSummaryError(null)
    try {
      const res = await summarizeFile(meta.id)
      setSummary(res)
    } catch (e) {
      setSummaryError(e instanceof Error ? e.message : 'AI 摘要失败')
    } finally {
      setSummaryLoading(false)
    }
  }, [meta, summaryLoading])

  const handleReExtract = useCallback(async () => {
    if (!meta || qualityLoading) return
    setQualityLoading(true)
    try {
      await reExtractFile(meta.id)
      const p = await getFilePreview(meta.id)
      setPreview(p)
    } catch (e) {
      toast(e instanceof Error ? e.message : t('quality_re_extract'))
    } finally {
      setQualityLoading(false)
    }
  }, [meta, qualityLoading, t])

  /** 导出文本：`.txt` = 纯正文（与库中逐字节相同），`.md` = 带时间/说话人标签。 */
  const handleExport = useCallback(async (format: ExportFormat) => {
    if (!meta || exporting) return
    setExportOpen(false)
    setExporting(true)
    try {
      const { content } = await exportFileText(meta.id, format)
      if (!content.trim()) {
        toast(t('no_text_to_export'))
        return
      }
      const base = meta.file_name.replace(/\.[^.]+$/, '') || 'export'
      const ext = format === 'tagged' ? 'md' : 'txt'
      await exportFile(content, `${base}.${ext}`, format === 'tagged' ? 'text/markdown' : 'text/plain')
      toast(t('export_done'))
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e))
    } finally {
      setExporting(false)
    }
  }, [meta, exporting, t])

  /** 非音频只有一种格式 → 直接导出；音频才弹格式菜单。 */
  const onExportClick = useCallback(() => {
    if (preview?.file_type !== 'audio') {
      void handleExport('raw')
      return
    }
    setExportOpen(v => !v)
  }, [preview?.file_type, handleExport])

  useEffect(() => {
    if (!exportOpen) return
    const close = () => setExportOpen(false)
    document.addEventListener('click', close)
    return () => document.removeEventListener('click', close)
  }, [exportOpen])

  const handleResizeStart = useCallback((e: React.MouseEvent) => {
    e.preventDefault()
    const startX = e.clientX
    const startWidth = clampPreviewWidth(width, window.innerWidth)
    dragWidthRef.current = startWidth
    setDragWidth(startWidth)

    const onMouseMove = (ev: MouseEvent) => {
      const next = clampPreviewWidth(startWidth + (startX - ev.clientX), window.innerWidth)
      dragWidthRef.current = next
      setDragWidth(next)
    }

    const onMouseUp = () => {
      if (dragWidthRef.current != null) setWidth(dragWidthRef.current)
      dragWidthRef.current = null
      setDragWidth(null)
      document.removeEventListener('mousemove', onMouseMove)
      document.removeEventListener('mouseup', onMouseUp)
    }

    document.addEventListener('mousemove', onMouseMove)
    document.addEventListener('mouseup', onMouseUp)
  }, [width, setWidth])

  const resetWidth = useCallback(() => setWidth(DEFAULT_PREVIEW_WIDTH), [setWidth])

  const copyPath = useCallback(() => {
    const p = meta?.path ?? path
    if (!p) return
    navigator.clipboard.writeText(p).then(() => toast(t('copied_path'))).catch(e => console.warn('复制失败:', e))
  }, [meta, path, t])

  const scrollToMatch = useCallback((dir: 'prev' | 'next') => {
    if (!contentRef.current || matchCount === 0) return
    const marks = contentRef.current.querySelectorAll('mark')
    if (marks.length === 0) return

    let nextIdx: number
    if (dir === 'next') {
      nextIdx = (matchIndex + 1) % marks.length
    } else {
      nextIdx = (matchIndex - 1 + marks.length) % marks.length
    }
    setMatchIndex(nextIdx)
    marks[nextIdx]!.scrollIntoView({ behavior: 'smooth', block: 'center' })
  }, [matchCount, matchIndex])

  const body = (
    <>
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-2 border-b border-gray-200 dark:border-gray-800 shrink-0">
        <h3 className="min-w-0 flex-1 truncate text-sm font-medium text-gray-900 dark:text-gray-100" title={displayName}>
          {displayName}
        </h3>
        <div className="flex items-center gap-1 shrink-0">
          {/* 字号：A− / 当前值（点击复位）/ A+ */}
          {hasTarget && (
            <div className="flex items-center rounded-md border border-gray-200 dark:border-gray-700 overflow-hidden text-[11px] leading-none select-none">
              <button
                onClick={() => setFontSize(stepFontSize(activeFontSize, -1))}
                disabled={activeFontSize <= stepFontSize(activeFontSize, -1)}
                title={t('preview_font_smaller')}
                className="px-1.5 py-1 text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
              >
                A−
              </button>
              <button
                onClick={() => setFontSize(DEFAULT_PREVIEW_FONT_SIZE)}
                title={t('preview_font_reset')}
                className="px-1.5 py-1 border-x border-gray-200 dark:border-gray-700 tabular-nums text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors"
              >
                {activeFontSize}
              </button>
              <button
                onClick={() => setFontSize(stepFontSize(activeFontSize, 1))}
                disabled={activeFontSize >= stepFontSize(activeFontSize, 1)}
                title={t('preview_font_larger')}
                className="px-1.5 py-1 text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
              >
                A+
              </button>
            </div>
          )}
          {hasTarget && !isMobile && (
            <button
              onClick={() => setFullscreen(v => !v)}
              className="p-1 rounded hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors"
              title={fullscreen ? t('shrink_panel') : t('expand_panel')}
            >
              <svg
                className="size-4 text-gray-400"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                {fullscreen
                  ? <><path d="M8 3v3a2 2 0 0 1-2 2H3m18 0h-3a2 2 0 0 1-2-2V3m0 18v-3a2 2 0 0 1 2-2h3M3 16h3a2 2 0 0 1 2 2v3" /></>
                  : <><path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3" /></>
                }
              </svg>
            </button>
          )}
          {hasTarget && (
            <button onClick={onClose} className="p-1 rounded hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors" title={t('close')}>
              <XIcon className="size-4 text-gray-400" />
            </button>
          )}
        </div>
      </div>

      {!hasTarget ? (
        <div className="flex-1 flex items-center justify-center p-6 text-sm text-gray-400 dark:text-gray-500">
          {t('select_file_preview')}
        </div>
      ) : (
        <>
          {/* 唯一的滚动容器：正文再长也在这里滚，内层不再套 max-height */}
          <div ref={contentRef} className="flex-1 overflow-y-auto overscroll-contain">
            {loading && (
              <div className="flex items-center justify-center py-12">
                <LoadingSpinner className="size-5 text-gray-400" />
              </div>
            )}

            {error && (
              <div className="p-4 text-sm text-red-600 dark:text-red-400">
                {error}
              </div>
            )}

            {!loading && !error && (
              <>
                {(meta || path) && (
                  <div className="px-4 py-3 space-y-2 border-b border-gray-100 dark:border-gray-800">
                    {preview?.file_type === 'pdf' && (
                      <>
                        <div className="flex items-center gap-1.5 text-xs font-medium text-blue-600 dark:text-blue-400">
                          <span className="text-sm">📄</span>
                          <span>{t('pdf_file')}</span>
                        </div>
                        <div className="text-xs font-medium text-gray-700 dark:text-gray-300 pt-1 pb-2">
                          {t('ocr_text_content')}
                        </div>
                      </>
                    )}
                    <MetaRow label={t('path')} value={meta?.path ?? path ?? ''} />
                    {meta && (
                      <>
                        <MetaRow label={t('size')} value={formatSize(meta.file_size)} />
                        <MetaRow label={t('modified')} value={formatTime(meta.mtime)} />
                        <MetaRow label={t('type')} value={meta.file_ext.toUpperCase()} />
                      </>
                    )}
                  </div>
                )}

                {meta && (
                  <div className="px-4 py-3 space-y-2 border-b border-gray-100 dark:border-gray-800">
                    <div className="flex items-center gap-2">
                      <button
                        onClick={handleSummarize}
                        disabled={summaryLoading || !aiCap.llm}
                        title={aiCap.llm ? undefined : t('ai_llm_unavailable')}
                        className="px-2.5 py-1 text-xs font-medium text-purple-600 dark:text-purple-400 bg-purple-50 dark:bg-purple-900/20 border border-purple-200 dark:border-purple-800/40 rounded-md hover:bg-purple-100 dark:hover:bg-purple-900/40 transition-colors disabled:opacity-40 disabled:cursor-not-allowed shrink-0"
                      >
                        {summaryLoading ? <LoadingSpinner className="size-3.5" /> : '✦'}
                        <span className="ml-1">{t('ai_summarize')}</span>
                      </button>
                    </div>
                    {summaryError && (
                      <p className="text-xs text-red-600 dark:text-red-400">{summaryError}</p>
                    )}
                    {summary && (
                      <div className="text-sm text-gray-700 dark:text-gray-300 bg-purple-50 dark:bg-purple-900/10 border border-purple-100 dark:border-purple-800/30 rounded p-2.5">
                        {summary.summary}
                      </div>
                    )}
                  </div>
                )}

                {preview?.file_type === 'image' && preview.image_path && (
                  <div className="flex items-center justify-center p-4 border-b border-gray-100 dark:border-gray-800 relative overflow-hidden">
                    <div className="absolute top-4 left-4 flex gap-1 z-10">
                      <button
                        onClick={() => setScale(Math.max(0.25, scale - 0.25))}
                        className="px-2 py-0.5 text-xs font-medium bg-white/90 dark:bg-gray-800/90 backdrop-blur-sm border border-gray-300 dark:border-gray-600 rounded hover:bg-gray-50 dark:hover:bg-gray-700 transition-colors"
                        title={t('zoom_out')}
                      >
                        [-]
                      </button>
                      <button
                        onClick={() => setScale(1)}
                        className="px-2 py-0.5 text-xs font-medium bg-white/90 dark:bg-gray-800/90 backdrop-blur-sm border border-gray-300 dark:border-gray-600 rounded hover:bg-gray-50 dark:hover:bg-gray-700 transition-colors"
                        title="100%"
                      >
                        {Math.round(scale * 100)}%
                      </button>
                      <button
                        onClick={() => setScale(Math.min(3, scale + 0.25))}
                        className="px-2 py-0.5 text-xs font-medium bg-white/90 dark:bg-gray-800/90 backdrop-blur-sm border border-gray-300 dark:border-gray-600 rounded hover:bg-gray-50 dark:hover:bg-gray-700 transition-colors"
                        title={t('zoom_in')}
                      >
                        [+]
                      </button>
                    </div>
                    <img
                      src={imageSrc(preview, resolveAssetUrl)}
                      alt=""
                      className="max-w-full max-h-96 object-contain rounded transition-transform duration-200"
                      style={{ transform: `scale(${scale})` }}
                    />
                  </div>
                )}

                {textContent && (
                  <pre
                    className="px-4 py-3 text-gray-800 dark:text-gray-200 whitespace-pre-wrap break-words font-mono"
                    style={{ fontSize: activeFontSize, lineHeight: 1.7 }}
                  >
                    {highlightText(shownText, searchQuery)}
                  </pre>
                )}

                {truncated && (
                  <div className="px-4 pb-3 text-xs text-amber-600 dark:text-amber-400">
                    {t('truncated_notice')}
                  </div>
                )}

                {preview?.ocr_used && (
                  <div className="px-4 py-2 text-xs text-amber-600 dark:text-amber-400 bg-amber-50 dark:bg-amber-900/20 border-t border-amber-200 dark:border-amber-800/30">
                    {t('ocr_applied_notice')}
                  </div>
                )}

                {preview && (meta || preview.char_count > 0) && (
                  <div className="px-4 py-3 space-y-2 border-t border-gray-100 dark:border-gray-800">
                    {meta && (
                      <MetaRow
                        label={t('quality_section')}
                        value={
                          preview.quality_score != null
                            ? `${(preview.quality_score * 100).toFixed(0)}%`
                            : t('quality_not_evaluated')
                        }
                        valueClassName={
                          preview.quality_score == null
                            ? 'text-gray-400 dark:text-gray-500'
                            : preview.quality_score > 0.75
                              ? 'text-green-600 dark:text-green-400'
                              : preview.quality_score >= 0.5
                                ? 'text-yellow-600 dark:text-yellow-400'
                                : 'text-red-600 dark:text-red-400'
                        }
                      />
                    )}
                    {meta && (() => {
                      const flags = parseQualityFlags(preview.quality_flags)
                      return flags.length > 0 ? (
                        <div className="flex flex-wrap gap-1 pt-0.5">
                          {flags.map(f => (
                            <span key={f} className="px-1.5 py-0.5 text-[10px] font-medium text-amber-700 dark:text-amber-300 bg-amber-50 dark:bg-amber-900/30 border border-amber-200 dark:border-amber-800/40 rounded">
                              {f}
                            </span>
                          ))}
                        </div>
                      ) : null
                    })()}
                    {meta && (
                      <button
                        onClick={handleReExtract}
                        disabled={qualityLoading}
                        className="px-2.5 py-1 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800/40 rounded-md hover:bg-blue-100 dark:hover:bg-blue-900/40 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                      >
                        {qualityLoading ? <LoadingSpinner className="size-3.5" /> : null}
                        <span className="ml-1">{qualityLoading ? t('quality_re_extracting') : t('quality_re_extract')}</span>
                      </button>
                    )}
                    {preview.char_count > 0 && (
                      <div className="text-xs text-gray-400 dark:text-gray-500">
                        {t('characters_count', { count: preview.char_count })}
                      </div>
                    )}
                  </div>
                )}

                {!textContent && preview && preview.file_type !== 'image' && (
                  <p className="px-4 py-3 text-sm text-gray-400 dark:text-gray-500">{t('no_preview_available')}</p>
                )}
              </>
            )}
          </div>

          {/* Match navigation */}
          {matchCount > 0 && (
            <div className="flex items-center justify-center gap-3 px-4 py-2 border-t border-gray-200 dark:border-gray-800 bg-gray-50 dark:bg-gray-800/50 shrink-0">
              <button
                onClick={() => scrollToMatch('prev')}
                className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 transition-colors"
                title={t('previous_match')}
              >
                <svg className="size-3.5 text-gray-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="m18 15-6-6-6 6" />
                </svg>
              </button>
              <span className="text-xs text-gray-500 dark:text-gray-400 tabular-nums">
                {matchIndex + 1} / {matchCount}
              </span>
              <button
                onClick={() => scrollToMatch('next')}
                className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 transition-colors"
                title={t('next_match')}
              >
                <svg className="size-3.5 text-gray-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="m6 9 6 6 6-6" />
                </svg>
              </button>
            </div>
          )}

          {/* Action bar */}
          {(meta || path || textContent) && (
            <div className="flex flex-wrap gap-2 px-3 py-2 border-t border-gray-200 dark:border-gray-800 shrink-0">
              {(meta || path) && (
                <button
                  onClick={copyPath}
                  className="flex-1 min-w-[80px] px-2 py-1.5 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                >
                  {t('copy_path')}
                </button>
              )}
              {meta && (
                <button
                  onClick={() => revealInFolder(meta.id)}
                  className="flex-1 min-w-[80px] px-2 py-1.5 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                >
                  {t('show_in_folder')}
                </button>
              )}
              {textContent && (
                <button
                  onClick={() => {
                    navigator.clipboard.writeText(textContent)
                      .then(() => toast(t('copied_text')))
                      .catch(e => console.warn('复制失败:', e))
                  }}
                  className="flex-1 min-w-[80px] px-2 py-1.5 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                >
                  {t('copy_all_text')}
                </button>
              )}
              {textContent && meta && (
                <span className="relative flex-1 min-w-[80px]">
                  <button
                    onClick={e => { e.stopPropagation(); onExportClick() }}
                    disabled={exporting}
                    className="w-full px-2 py-1.5 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                  >
                    {exporting ? <LoadingSpinner className="size-3" /> : t('export_text')}
                    {preview?.file_type === 'audio' ? ' ▾' : ''}
                  </button>
                  {exportOpen && preview?.file_type === 'audio' && (
                    <div
                      className="absolute bottom-full right-0 mb-1 z-20 w-52 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-lg py-1"
                      onClick={e => e.stopPropagation()}
                    >
                      <button
                        onClick={() => void handleExport('raw')}
                        className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
                      >
                        {t('export_raw')}
                      </button>
                      <button
                        onClick={() => void handleExport('tagged')}
                        disabled={!preview?.has_timeline}
                        title={preview?.has_timeline ? undefined : t('export_tagged_no_timeline')}
                        className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
                      >
                        {t('export_tagged')}
                      </button>
                    </div>
                  )}
                </span>
              )}
              {meta && (
                <button
                  onClick={() => openFile(meta.id)}
                  className="flex-1 min-w-[80px] px-2 py-1.5 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 border border-blue-200 dark:border-blue-800/40 rounded-md hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
                >
                  {t('open')}
                </button>
              )}
            </div>
          )}
        </>
      )}
    </>
  )

  if (isMobile) {
    if (!hasTarget) return null
    return createPortal(
      <div className="preview-panel fixed inset-0 z-50 flex flex-col bg-white dark:bg-gray-900 lg:hidden">
        {body}
      </div>,
      document.body,
    )
  }

  if (!hasTarget && !collapsible) return null

  if (fullscreen) {
    return (
      <div
        className="preview-panel fixed inset-y-0 right-0 z-40 flex flex-col bg-white dark:bg-gray-900 border-l border-gray-200 dark:border-gray-800 shadow-2xl"
        style={{ width: 'min(92vw, 1200px)' }}
      >
        {body}
      </div>
    )
  }

  if (collapsible && collapsed) {
    return (
      <button
        onClick={() => setCollapsed(false)}
        title={t('show_preview')}
        className="w-5 shrink-0 self-stretch flex items-center justify-center text-gray-400 dark:text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800 hover:text-gray-600 dark:hover:text-gray-300 transition-colors border-l border-gray-200 dark:border-gray-800"
      >
        ◀
      </button>
    )
  }

  return (
    <div className="flex shrink-0">
      <div
        className="preview-panel relative flex flex-col bg-white dark:bg-gray-900 border-l border-gray-200 dark:border-gray-800"
        style={{ width: effectiveWidth }}
      >
        {/* 可拖拽的宽度手柄（左边缘，双击复位） */}
        <div
          className="absolute left-0 top-0 bottom-0 z-10 w-1.5 -ml-0.5 cursor-col-resize hover:bg-blue-500/50 active:bg-blue-500 transition-colors"
          onMouseDown={handleResizeStart}
          onDoubleClick={resetWidth}
          title={t('preview_drag_width')}
        />
        {body}
      </div>
      {collapsible && (
        <button
          onClick={() => setCollapsed(true)}
          title={t('hide_preview')}
          className="w-5 shrink-0 self-stretch flex items-center justify-center text-gray-400 dark:text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800 hover:text-gray-600 dark:hover:text-gray-300 transition-colors border-l border-gray-200 dark:border-gray-800"
        >
          ▶
        </button>
      )}
    </div>
  )
}

function MetaRow({ label, value, valueClassName }: { label: string; value: string; valueClassName?: string }) {
  return (
    <div className="flex items-start gap-2">
      <span className="text-xs font-medium text-gray-500 dark:text-gray-400 shrink-0 w-16">{label}</span>
      <span className={`text-xs break-all ${valueClassName ?? 'text-gray-700 dark:text-gray-300'}`}>{value}</span>
    </div>
  )
}
