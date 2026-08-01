import { useEffect, useState, useRef, useMemo, useCallback } from 'react'
import { convertFileSrc } from '@tauri-apps/api/core'
import { getFile, getFilePreview, openFile, revealInFolder, type FileDetail, type FilePreview } from '../api/files'
import { useI18n } from '../i18n'
import { XIcon, LoadingSpinner } from '../icons'

interface PreviewPanelProps {
  fileId: string | null
  searchQuery: string
  onClose: () => void
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

function formatTime(ts: number): string {
  return new Date(ts / 1000).toLocaleString()
}

function highlightText(text: string, query: string): React.ReactNode[] {
  if (!query.trim()) return [text]
  const terms = query.split(/\s+/).filter(t => t.length > 0)
  if (terms.length === 0) return [text]
  const termSet = new Set(terms.map(t => t.toLowerCase()))
  const regex = new RegExp(`(${terms.map(t => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})`, 'gi')
  const parts = text.split(regex)
  return parts.map((part, i) =>
    part.length > 0 && termSet.has(part.toLowerCase())
      ? <mark key={i} className="bg-yellow-200 dark:bg-yellow-700/50 rounded px-0.5">{part}</mark>
      : part,
  )
}

function countMatches(text: string, query: string): number {
  if (!query.trim()) return 0
  const terms = query.split(/\s+/).filter(t => t.length > 0)
  if (terms.length === 0) return 0
  // Single scan: build regex once, then match all at once
  const regex = new RegExp(terms.map(t => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|'), 'gi')
  const matches = text.match(regex)
  return matches ? matches.length : 0
}

export default function PreviewPanel({ fileId, searchQuery, onClose }: PreviewPanelProps) {
  const { t } = useI18n()
  const [meta, setMeta] = useState<FileDetail | null>(null)
  const [preview, setPreview] = useState<FilePreview | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [width, setWidth] = useState(400)
  const [fullscreen, setFullscreen] = useState(false)
  const [matchIndex, setMatchIndex] = useState(0)
  const [scale, setScale] = useState(1)
  const contentRef = useRef<HTMLDivElement>(null)
  const resizingRef = useRef(false)

  const effectiveWidth = fullscreen ? Math.max(400, window.innerWidth * 0.6) : width

  useEffect(() => {
    if (!fileId) {
      setMeta(null)
      setPreview(null)
      setError(null)
      setMatchIndex(0)
      return
    }

    let cancelled = false
    setLoading(true)
    setError(null)
    setMatchIndex(0)

    Promise.all([getFile(fileId), getFilePreview(fileId)])
      .then(([f, p]) => {
        if (!cancelled) {
          setMeta(f)
          setPreview(p)
        }
      })
        .catch(e => {
          if (!cancelled) setError(e instanceof Error ? e.message : t('failed_load_preview'))
        })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })

    return () => { cancelled = true }
  }, [fileId])

  const textContent = preview?.content ?? ''
  const matchCount = useMemo(() => countMatches(textContent, searchQuery), [textContent, searchQuery])

  const handleResizeStart = useCallback((e: React.MouseEvent) => {
    e.preventDefault()
    resizingRef.current = true
    const startX = e.clientX
    const startWidth = effectiveWidth

    const onMouseMove = (ev: MouseEvent) => {
      if (!resizingRef.current) return
      const newWidth = startWidth + (startX - ev.clientX)
      setWidth(Math.max(250, Math.min(800, newWidth)))
      setFullscreen(false)
    }

    const onMouseUp = () => {
      resizingRef.current = false
      document.removeEventListener('mousemove', onMouseMove)
      document.removeEventListener('mouseup', onMouseUp)
    }

    document.addEventListener('mousemove', onMouseMove)
    document.addEventListener('mouseup', onMouseUp)
  }, [effectiveWidth])

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
    marks[nextIdx].scrollIntoView({ behavior: 'smooth', block: 'center' })
  }, [matchCount, matchIndex])

  if (!fileId) return null

  return (
    <div
      className="shrink-0 border-l border-gray-200 dark:border-gray-800 bg-white dark:bg-gray-900 flex flex-col relative"
      style={{ width: effectiveWidth }}
    >
      {/* Resize handle */}
      <div
        className="absolute left-0 top-0 bottom-0 w-1 cursor-col-resize hover:bg-blue-500/50 active:bg-blue-500 transition-colors z-10"
        onMouseDown={handleResizeStart}
      />

      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-800">
        <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">
          {meta?.file_name ?? t('preview')}
        </h3>
        <div className="flex items-center gap-1">
          <button
            onClick={() => setFullscreen(v => !v)}
            className="p-0.5 rounded hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors"
            title={fullscreen ? t('shrink_panel') : t('expand_panel')}
          >
            <svg
              className="size-3.5 text-gray-400"
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
          <button onClick={onClose} className="p-0.5 rounded hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors">
            <XIcon className="size-3.5 text-gray-400" />
          </button>
        </div>
      </div>

      {/* Content */}
      <div ref={contentRef} className="flex-1 overflow-y-auto">
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

        {meta && !loading && !error && (
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
            <MetaRow label={t('path')} value={meta.path} />
            <MetaRow label={t('size')} value={formatSize(meta.file_size)} />
            <MetaRow label={t('modified')} value={formatTime(meta.mtime)} />
            <MetaRow label={t('type')} value={meta.file_ext.toUpperCase()} />
          </div>
        )}

        {preview?.file_type === 'image' && preview?.image_path && !loading && (
          <div className="flex items-center justify-center p-4 border-b border-gray-100 dark:border-gray-800 relative">
            {/* Zoom controls */}
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
              src={convertFileSrc(preview.image_path)}
              alt=""
              className="max-w-full max-h-96 object-contain rounded transition-transform duration-200"
              style={{ transform: `scale(${scale})` }}
            />
          </div>
        )}

        {textContent && !loading && (
          <div className="px-4 py-3">
            {textContent.length > 50000 ? (
              <>
                <pre className="px-4 py-2 text-xs text-gray-700 dark:text-gray-300 whitespace-pre-wrap font-mono leading-relaxed bg-gray-50 dark:bg-gray-800/50 rounded border border-gray-200 dark:border-gray-700 mb-1 max-h-96 overflow-y-auto">
                  {highlightText(textContent.substring(0, 50000), searchQuery)}
                </pre>
                <div className="text-xs text-gray-500 dark:text-gray-400 py-1">
                  {t('truncated_notice')}
                </div>
              </>
            ) : (
              <pre className="px-4 py-2 text-xs text-gray-700 dark:text-gray-300 whitespace-pre-wrap font-mono leading-relaxed">
                {highlightText(textContent, searchQuery)}
              </pre>
            )}
          </div>
        )}

        {preview?.ocr_used && (
          <div className="px-4 py-2 text-xs text-amber-600 dark:text-amber-400 bg-amber-50 dark:bg-amber-900/20 border-t border-amber-200 dark:border-amber-800/30">
            {t('ocr_applied_notice')}
          </div>
        )}
      </div>

      {/* Match navigation */}
      {matchCount > 0 && (
        <div className="flex items-center justify-center gap-3 px-4 py-2 border-t border-gray-200 dark:border-gray-800 bg-gray-50 dark:bg-gray-800/50">
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
      {meta && (
        <div className="flex gap-2 px-4 py-2 border-t border-gray-200 dark:border-gray-800">
          <button
            onClick={() => navigator.clipboard.writeText(meta.path)}
            className="flex-1 px-2 py-1.5 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
          >
            {t('copy_path')}
          </button>
          <button
            onClick={() => revealInFolder(meta.id)}
            className="flex-1 px-2 py-1.5 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
          >
            {t('show_in_folder')}
          </button>
          <button
            onClick={() => openFile(meta.id)}
            className="flex-1 px-2 py-1.5 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 border border-blue-200 dark:border-blue-800/40 rounded-md hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
          >
            {t('open')}
          </button>
        </div>
      )}
    </div>
  )
}

function MetaRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-start gap-2">
      <span className="text-xs font-medium text-gray-500 dark:text-gray-400 shrink-0 w-16">{label}</span>
      <span className="text-xs text-gray-700 dark:text-gray-300 break-all">{value}</span>
    </div>
  )
}