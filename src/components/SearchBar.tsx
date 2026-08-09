import { useCallback, useRef, useState } from 'react'
import { useI18n } from '../i18n'
import { SearchIcon, XIcon, LoadingSpinner } from '../icons'

interface SearchBarProps {
  query: string
  loading: boolean
  onQueryChange: (q: string) => void
  onSubmit: () => void
}

export default function SearchBar({
  query,
  loading,
  onQueryChange,
  onSubmit,
}: SearchBarProps) {
  const { t } = useI18n()
  const inputRef = useRef<HTMLInputElement>(null)
  const [focused, setFocused] = useState(false)

  const handleInput = useCallback((value: string) => {
    onQueryChange(value)
  }, [onQueryChange])

  const clear = useCallback(() => {
    onQueryChange('')
    inputRef.current?.focus()
  }, [onQueryChange])

  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      e.preventDefault()
      onSubmit()
    }
  }, [onSubmit])

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
    </div>
  )
}
