import { useCallback, useEffect, useRef, useState } from 'react'
import { getSearchHistory, clearSearchHistory, type SearchHistoryEntry } from '../api/search'
import { useI18n } from '../i18n'
import { SearchIcon, XIcon, LoadingSpinner } from '../icons'

interface SearchBarProps {
  query: string
  loading: boolean
  suggestions: string[]
  onQueryChange: (q: string) => void
  onFetchSuggestions: (prefix: string) => void
  onClearSuggestions: () => void
}

export default function SearchBar({
  query,
  loading,
  suggestions,
  onQueryChange,
  onFetchSuggestions,
  onClearSuggestions,
}: SearchBarProps) {
  const { t } = useI18n()
  const inputRef = useRef<HTMLInputElement>(null)
  const [focused, setFocused] = useState(false)
  const [selectedIdx, setSelectedIdx] = useState(-1)
  const [history, setHistory] = useState<SearchHistoryEntry[]>([])

  useEffect(() => {
    getSearchHistory().then(setHistory).catch(() => {})
  }, [])

  useEffect(() => {
    if (query) {
      getSearchHistory().then(setHistory).catch(() => {})
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query])

  const handleClearHistory = useCallback(async () => {
    try {
      await clearSearchHistory()
      setHistory([])
    } catch {
      /* ignore clear failure */
    }
  }, [])

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault()
        inputRef.current?.focus()
      }
    }
    document.addEventListener('keydown', handler)
    return () => document.removeEventListener('keydown', handler)
  }, [])

  const handleInput = useCallback((value: string) => {
    onQueryChange(value)
    onFetchSuggestions(value)
    setSelectedIdx(-1)
  }, [onQueryChange, onFetchSuggestions])

  const clear = useCallback(() => {
    onQueryChange('')
    onClearSuggestions()
    setSelectedIdx(-1)
    inputRef.current?.focus()
  }, [onQueryChange, onClearSuggestions])

  const selectSuggestion = useCallback((s: string) => {
    onQueryChange(s)
    onClearSuggestions()
    setSelectedIdx(-1)
    inputRef.current?.focus()
  }, [onQueryChange, onClearSuggestions])

  const selectHistory = useCallback((entry: SearchHistoryEntry) => {
    onQueryChange(entry.query)
    onClearSuggestions()
    setSelectedIdx(-1)
    inputRef.current?.focus()
  }, [onQueryChange, onClearSuggestions])

  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    const dropdown = query.trim().length === 0
      ? history
      : suggestions.length > 0
        ? [...suggestions, ...history]
        : []

    if (!dropdown.length) return

    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setSelectedIdx(i => Math.min(i + 1, dropdown.length - 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setSelectedIdx(i => Math.max(i - 1, -1))
    } else if (e.key === 'Enter' && selectedIdx >= 0) {
      e.preventDefault()
      const item = dropdown[selectedIdx]
      if (typeof item === 'string') {
        selectSuggestion(item)
      } else {
        selectHistory(item)
      }
    } else if (e.key === 'Escape') {
      onClearSuggestions()
      setSelectedIdx(-1)
    }
  }, [suggestions, history, query, selectedIdx, selectSuggestion, selectHistory, onClearSuggestions])

  const showSuggestions = focused && suggestions.length > 0 && query.trim().length > 0
  const showHistory = focused && history.length > 0

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
          onFocus={() => setFocused(true)}
          onBlur={() => setTimeout(() => setFocused(false), 150)}
          placeholder={`${t('search_placeholder')} (⌘K)`}
          aria-label={t('search')}
          data-search-input="true"
          className="w-full pl-9 pr-8 py-2.5 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-sm text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent transition-colors"
        />
        {query && (
          <button onClick={clear} className="absolute right-3 p-0.5 rounded hover:bg-gray-200 dark:hover:bg-gray-700 transition-colors" aria-label={t('clear_search')}>
            <XIcon className="size-3.5 text-gray-400" />
          </button>
        )}
      </div>

      {(showSuggestions || showHistory) && (
        <div className="absolute top-full left-0 right-0 mt-1 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-lg z-50 overflow-hidden">
          {showSuggestions && suggestions.map((s, i) => (
            <button
              key={s}
              onMouseDown={() => selectSuggestion(s)}
              className={`w-full text-left px-3 py-2 text-sm transition-colors ${
                i === selectedIdx
                  ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-700 dark:text-blue-300'
                  : 'text-gray-700 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-750'
              }`}
            >
              {s}
            </button>
          ))}
          {showSuggestions && showHistory && (
            <div className="border-t border-gray-200 dark:border-gray-700" />
          )}
          {showHistory && (
            <>
              <div className="px-3 py-1.5 text-xs font-medium text-gray-400 dark:text-gray-500 uppercase tracking-wider flex items-center justify-between">
                <span>{t('recent_searches')}</span>
                <button
                  onClick={handleClearHistory}
                  className="text-gray-400 hover:text-red-500 dark:hover:text-red-400 transition-colors normal-case"
                  title={t('clear_history')}
                >
                  {t('clear_history')}
                </button>
              </div>
              {history.map((entry, i) => (
                <button
                  key={entry.id}
                  onMouseDown={() => selectHistory(entry)}
                  className={`w-full text-left px-3 py-2 text-sm transition-colors flex items-center justify-between ${
                    i === (showSuggestions ? selectedIdx - suggestions.length : selectedIdx)
                      ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-700 dark:text-blue-300'
                      : 'text-gray-700 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-750'
                  }`}
                >
                  <span className="truncate">{entry.query}</span>
                  <span className="text-xs text-gray-400 shrink-0 ml-2">{t('results_count', { total: entry.result_count })}</span>
                </button>
              ))}
            </>
          )}
        </div>
      )}
    </div>
  )
}