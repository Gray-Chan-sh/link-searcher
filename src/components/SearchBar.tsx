import { useCallback, useEffect, useRef, useState } from 'react'
import { useI18n } from '../i18n'
import { SearchIcon, XIcon, LoadingSpinner } from '../icons'

interface SearchBarProps {
  query: string
  loading: boolean
  suggestions?: string[]
  onQueryChange: (q: string) => void
  onSubmit: () => void
  onFetchSuggestions?: (prefix: string) => void
  onClearSuggestions?: () => void
  onPickSuggestion?: (s: string) => void
}

export default function SearchBar({
  query,
  loading,
  suggestions,
  onQueryChange,
  onSubmit,
  onFetchSuggestions,
  onClearSuggestions,
  onPickSuggestion,
}: SearchBarProps) {
  const { t } = useI18n()
  const inputRef = useRef<HTMLInputElement>(null)
  const [open, setOpen] = useState(false)
  const [activeIdx, setActiveIdx] = useState(-1)

  const handleInput = useCallback((value: string) => {
    onQueryChange(value)
    onFetchSuggestions?.(value)
    setOpen(true)
    setActiveIdx(-1)
  }, [onQueryChange, onFetchSuggestions])

  const close = useCallback(() => {
    setOpen(false)
    setActiveIdx(-1)
    onClearSuggestions?.()
  }, [onClearSuggestions])

  const pick = useCallback((s: string) => {
    onPickSuggestion?.(s)
    close()
    onSubmit()
  }, [onPickSuggestion, close, onSubmit])

  const clear = useCallback(() => {
    onQueryChange('')
    close()
    inputRef.current?.focus()
  }, [onQueryChange, close])

  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (!open || !suggestions?.length) {
      if (e.key === 'Enter') {
        e.preventDefault()
        onSubmit()
      }
      if (e.key === 'Escape') close()
      return
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setActiveIdx(i => (i + 1) % suggestions.length)
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setActiveIdx(i => (i <= 0 ? suggestions.length - 1 : i - 1))
    } else if (e.key === 'Enter') {
      e.preventDefault()
      if (activeIdx >= 0 && suggestions[activeIdx]) {
        pick(suggestions[activeIdx])
      } else {
        onSubmit()
        close()
      }
    } else if (e.key === 'Escape') {
      e.preventDefault()
      close()
    }
  }, [open, suggestions, activeIdx, onSubmit, close, pick])

  // ⌘K / Ctrl+K 全局快捷键聚焦搜索框
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault()
        inputRef.current?.focus()
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [])

  const showDropdown = open && !!suggestions?.length && query.trim().length > 0

  return (
    <div className="relative">
      <div className="relative flex items-center">
        {loading ? (
          <LoadingSpinner className="absolute left-3 size-4 text-gray-400" />
        ) : (
          <SearchIcon className="absolute left-3 size-4 text-gray-400" />
        )}
        <input
          ref={inputRef}
          type="text"
          value={query}
          onChange={e => handleInput(e.target.value)}
          onKeyDown={handleKeyDown}
          onFocus={() => { if (query.trim() && suggestions?.length) setOpen(true) }}
          onBlur={() => setTimeout(() => setOpen(false), 120)}
          placeholder={`${t('search_placeholder')} (⌘K)`}
          aria-label={t('search')}
          aria-expanded={showDropdown}
          data-search-input="true"
          className="w-full pl-9 pr-20 py-2.5 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-sm text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent transition-colors"
        />
        <div className="absolute right-2 top-1/2 -translate-y-1/2 flex items-center gap-0.5">
          <button
            onClick={onSubmit}
            disabled={loading}
            className="p-1.5 rounded hover:bg-blue-100 dark:hover:bg-blue-900/30 text-blue-600 dark:text-blue-400 transition-colors disabled:opacity-40"
            aria-label={t('search')}
          >
            <SearchIcon className="size-3.5" />
          </button>
          {query && (
            <button onClick={clear} className="p-0.5 rounded hover:bg-gray-200 dark:hover:bg-gray-700 transition-colors" aria-label={t('clear_search')}>
              <XIcon className="size-3.5 text-gray-400" />
            </button>
          )}
        </div>
      </div>
      {showDropdown && (
        <ul className="absolute left-0 right-0 mt-1 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-lg overflow-hidden z-30 text-sm max-h-64 overflow-y-auto">
          {suggestions.map((s, i) => (
            <li key={s}>
              <button
                type="button"
                onMouseDown={e => { e.preventDefault(); pick(s) }}
                onMouseEnter={() => setActiveIdx(i)}
                className={`w-full px-3 py-2 text-left truncate hover:bg-gray-100 dark:hover:bg-gray-700 ${
                  i === activeIdx ? 'bg-blue-50 dark:bg-blue-900/20 text-blue-700 dark:text-blue-300' : 'text-gray-700 dark:text-gray-300'
                }`}
              >
                {s}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
