import { useEffect, useRef, useState } from 'react'
import type { SearchHit } from '../api/search'
import { openFile, revealInFolder } from '../api/files'

const ITEM_HEIGHT = 72
const OVERSCAN = 5

interface ContextMenu {
  x: number
  y: number
  hit: SearchHit
}

interface ResultListProps {
  hits: SearchHit[]
  selectedId: string | null
  onSelect: (hit: SearchHit) => void
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

function formatTime(ts: number): string {
  const d = new Date(ts * 1000)
  return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' })
}

function highlightSnippet(snippet: string): React.ReactNode {
  const parts = snippet.split(/(<em>|<\/em>)/)
  let depth = 0
  return parts.map((part, i) => {
    if (part === '<em>') { depth++; return null }
    if (part === '</em>') { depth--; return null }
    if (!part) return null
    return depth > 0
      ? <mark key={i} className="bg-yellow-200 dark:bg-yellow-700/50 text-inherit rounded-sm px-0.5">{part}</mark>
      : <span key={i}>{part}</span>
  })
}

export default function ResultList({ hits, selectedId, onSelect }: ResultListProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const [visibleRange, setVisibleRange] = useState({ start: 0, end: 20 })
  const [contextMenu, setContextMenu] = useState<ContextMenu | null>(null)

  const handleContextMenu = (e: React.MouseEvent, hit: SearchHit) => {
    e.preventDefault()
    setContextMenu({ x: e.clientX, y: e.clientY, hit })
  }

  useEffect(() => {
    const close = () => setContextMenu(null)
    document.addEventListener('click', close)
    return () => document.removeEventListener('click', close)
  }, [])

  useEffect(() => {
    const container = containerRef.current
    if (!container) return

    const handleScroll = () => {
      const scrollTop = container.scrollTop
      const clientHeight = container.clientHeight
      const start = Math.max(0, Math.floor(scrollTop / ITEM_HEIGHT) - OVERSCAN)
      const end = Math.min(hits.length, Math.ceil((scrollTop + clientHeight) / ITEM_HEIGHT) + OVERSCAN)
      setVisibleRange({ start, end })
    }

    handleScroll()
    container.addEventListener('scroll', handleScroll, { passive: true })
    return () => container.removeEventListener('scroll', handleScroll)
  }, [hits.length])

  if (hits.length === 0) return null

  const totalHeight = hits.length * ITEM_HEIGHT

  return (
    <div ref={containerRef} className="flex-1 overflow-y-auto" tabIndex={0}>
      <div style={{ height: totalHeight, position: 'relative' }}>
        {hits.slice(visibleRange.start, visibleRange.end).map((hit, i) => {
          const actualIndex = visibleRange.start + i
          return (
            <button
              key={hit.file_id}
              onClick={() => onSelect(hit)}
              onContextMenu={(e) => handleContextMenu(e, hit)}
              className={`absolute left-0 right-0 w-full text-left px-4 py-3 transition-colors hover:bg-gray-50 dark:hover:bg-gray-800/50 ${
                selectedId === hit.file_id ? 'bg-blue-50 dark:bg-blue-900/20' : ''
              }`}
              style={{ top: actualIndex * ITEM_HEIGHT, height: ITEM_HEIGHT }}
            >
              <div className="flex items-center gap-2 mb-1">
                <span className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">
                  {hit.file_name}
                </span>
                <span className="text-xs text-gray-400 uppercase shrink-0">{hit.file_ext}</span>
                <span className="ml-auto text-xs text-gray-400 shrink-0">{formatTime(hit.mtime)}</span>
              </div>
              <p className="text-xs text-gray-500 dark:text-gray-400 leading-relaxed line-clamp-2 mb-1">
                {highlightSnippet(hit.snippet)}
              </p>
              <div className="flex items-center gap-3 text-xs text-gray-400">
                <span className="truncate">{hit.path}</span>
                <span className="shrink-0">{formatSize(hit.file_size)}</span>
                <span className="shrink-0">Score: {hit.score.toFixed(2)}</span>
              </div>
            </button>
          )
        })}
      </div>
      {contextMenu && (
        <div
          className="fixed z-50 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-lg py-1 min-w-[160px]"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          <button
            className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
            onClick={() => { openFile(contextMenu.hit.file_id); setContextMenu(null) }}
          >
            Open
          </button>
          <button
            className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
            onClick={() => { navigator.clipboard.writeText(contextMenu.hit.file_name); setContextMenu(null) }}
          >
            Copy Name
          </button>
          <button
            className="w-full px-3 py-1.5 text-left text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
            onClick={() => { revealInFolder(contextMenu.hit.file_id); setContextMenu(null) }}
          >
            Show in Folder
          </button>
        </div>
      )}
    </div>
  )
}
