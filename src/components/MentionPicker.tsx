import { useEffect, useRef, useState } from 'react'
import { searchFilePaths } from '../api/files'
import { FileTextIcon, FolderIcon } from '../icons'

interface MentionPickerProps {
  /** 当前在 @ 后面已输入的部分（不含 @ 符号） */
  query: string
  /** 选择器在输入框中的位置（px），用于定位弹出层 */
  position: { left: number; top: number } | null
  /** 用户选中一个路径时的回调 */
  onSelect: (path: string) => void
  /** 关闭选择器 */
  onClose: () => void
}

export default function MentionPicker({ query, position, onSelect, onClose }: MentionPickerProps) {
  const [items, setItems] = useState<string[]>([])
  const [highlight, setHighlight] = useState(0)
  const listRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!query) {
      setItems([])
      return
    }
    let cancelled = false
    searchFilePaths(query, 15).then(r => {
      if (!cancelled) {
        setItems(r)
        setHighlight(0)
      }
    }).catch(() => {})
    return () => { cancelled = true }
  }, [query])

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (items.length === 0) return
      if (e.key === 'ArrowDown') {
        e.preventDefault()
        setHighlight(i => Math.min(i + 1, items.length - 1))
      } else if (e.key === 'ArrowUp') {
        e.preventDefault()
        setHighlight(i => Math.max(i - 1, 0))
      } else if (e.key === 'Enter' && items[highlight]) {
        e.preventDefault()
        onSelect(items[highlight])
      } else if (e.key === 'Escape') {
        e.preventDefault()
        onClose()
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [items, highlight, onSelect, onClose])

  // 自动滚动高亮项
  useEffect(() => {
    const el = listRef.current?.children[highlight] as HTMLElement | undefined
    el?.scrollIntoView({ block: 'nearest' })
  }, [highlight])

  if (items.length === 0 || !position) return null

  return (
    <div
      className="fixed z-50 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-lg max-h-48 overflow-y-auto"
      style={{ left: position.left, top: position.top + 24 }}
      ref={listRef}
    >
      {items.map((path, i) => {
        const isFile = /\.\w{1,6}$/.test(path)
        return (
          <button
            key={path}
            type="button"
            className={`w-full text-left px-3 py-1.5 text-xs flex items-center gap-2 transition-colors ${
              i === highlight
                ? 'bg-purple-100 dark:bg-purple-900/40 text-purple-900 dark:text-purple-100'
                : 'text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700'
            }`}
            onMouseDown={e => { e.preventDefault(); onSelect(path) }}
            onMouseEnter={() => setHighlight(i)}
          >
            <span className="shrink-0">{isFile ? <FileTextIcon className="size-3 text-gray-400 dark:text-gray-500" /> : <FolderIcon className="size-3 text-amber-400 dark:text-amber-500" />}</span>
            <span className="truncate">{path}</span>
          </button>
        )
      })}
    </div>
  )
}