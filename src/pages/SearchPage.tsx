import { useEffect, useMemo, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { useSearch } from '../hooks/useSearch'
import { useDirs } from '../hooks/useDirs'
import { useI18n } from '../i18n'
import SearchBar from '../components/SearchBar'
import FilterPanel from '../components/FilterPanel'
import ResultList from '../components/ResultList'
import PreviewPanel from '../components/PreviewPanel'
import EmptyState from '../components/EmptyState'
import { ResultListSkeleton } from '../components/Skeleton'
import type { SearchHit } from '../api/search'
import { exportSearchResults } from '../api/search'
import { openFile, aiCapabilities, type AiCapabilities } from '../api/files'
import { SearchIcon } from '../icons'
import { save } from '@tauri-apps/plugin-dialog'
import { writeTextFile } from '@tauri-apps/plugin-fs'

export default function SearchPage() {
  const { t } = useI18n()
  const search = useSearch()
  const { dirs } = useDirs()
  const [selectedHit, setSelectedHit] = useState<SearchHit | null>(null)
  const [focusIndex, setFocusIndex] = useState(-1)
  const [showFilters, setShowFilters] = useState(true)
  const [exportMsg, setExportMsg] = useState<string | null>(null)
  const [aiCap, setAiCap] = useState<AiCapabilities>({ embedding: false, llm: false })
  const timersRef = useRef<ReturnType<typeof setTimeout>[]>([])

  useEffect(() => { aiCapabilities().then(setAiCap).catch(() => {}) }, [])

  useEffect(() => () => timersRef.current.forEach(clearTimeout), [])

  const totalPages = useMemo(
    () => Math.max(1, Math.ceil(search.total / search.pageSize)),
    [search.total, search.pageSize],
  )

  const handleExtToggle = (ext: string) => {
    const next = search.extFilter.includes(ext)
      ? search.extFilter.filter(e => e !== ext)
      : [...search.extFilter, ext]
    search.setExtFilter(next)
  }

const handleExport = async () => {
  try {
    const savedPath = await save({
      defaultPath: 'results.csv',
      filters: [{ name: 'CSV', extensions: ['csv'] }],
    })

    if (!savedPath) {
      setExportMsg(t('export_cancelled'))
      timersRef.current.push(setTimeout(() => setExportMsg(null), 3000))
      return
    }

    const content = await exportSearchResults(search.query, search.dirIds, search.extFilter, 'csv')
    await writeTextFile(savedPath, content)

    setExportMsg(t('saved_to', { path: savedPath }))
    timersRef.current.push(setTimeout(() => setExportMsg(null), 3000))
  } catch (e) {
    setExportMsg(t('export_failed', { error: e instanceof Error ? e.message : t('unknown_error') }))
    timersRef.current.push(setTimeout(() => setExportMsg(null), 5000))
  }
}

useEffect(() => {
  const handleKeyDown = (e: KeyboardEvent) => {
    // Don't override search input handling - if focused in search, let it process Enter itself
    const activeEl = document.activeElement
    if (activeEl?.closest('[data-search-input]')) return

    if (search.status !== 'success' || search.hits.length === 0) return

    if (e.key === 'ArrowDown') {
        e.preventDefault()
        const next = focusIndex + 1
        if (next < search.hits.length) {
          setFocusIndex(next)
          setSelectedHit(search.hits[next])
        }
      } else if (e.key === 'ArrowUp') {
        e.preventDefault()
        const prev = focusIndex - 1
        if (prev >= 0) {
          setFocusIndex(prev)
          setSelectedHit(search.hits[prev])
        }
      } else if (e.key === 'Enter' && focusIndex >= 0) {
        e.preventDefault()
        openFile(search.hits[focusIndex].file_id)
      }
    }

    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [search.status, search.hits, focusIndex])

  // Reset focus index when results change
  useEffect(() => {
    setFocusIndex(-1)
  }, [search.hits])

  return (
    <div className="flex h-full">
      {showFilters && (
        <FilterPanel
          dirs={dirs}
          dirPaths={search.dirPaths}
          extFilter={search.extFilter}
          onDirPathsChange={search.setDirPaths}
          onExtToggle={handleExtToggle}
          onClearFilters={() => {
            search.setDirIds([])
            search.setDirPaths([])
            search.setExtFilter([])
          }}
        />
      )}

      <div className="flex-1 flex flex-col min-w-0">
        <div className="px-4 pt-4 pb-2 space-y-3">
          <div className="flex items-center gap-2">
            <div className="flex-1">
              <SearchBar
                query={search.query}
                loading={search.status === 'loading'}
                suggestions={search.suggestions}
                onQueryChange={search.setQuery}
                onFetchSuggestions={search.fetchSuggestions}
                onClearSuggestions={search.clearSuggestions}
                onSubmit={search.submitSearch}
              />
            </div>
            <button
              onClick={() => setShowFilters(v => !v)}
              className="px-2.5 py-2 text-xs font-medium text-gray-500 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors shrink-0"
            >
              {t('filters')}
            </button>
            <button
              onClick={() => search.setSemantic(!search.semantic)}
              disabled={!aiCap.embedding}
              title={aiCap.embedding ? t('semantic_search') : t('ai_embedding_unavailable')}
              className={`px-2.5 py-2 text-xs font-medium rounded-lg border transition-colors shrink-0 ${
                !aiCap.embedding
                  ? 'text-gray-300 dark:text-gray-600 cursor-not-allowed'
                  : search.semantic
                  ? 'text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/20 border-blue-300 dark:border-blue-800'
                  : 'text-gray-500 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border-gray-200 dark:border-gray-700 hover:bg-gray-100 dark:hover:bg-gray-700'
              }`}
            >
              ✦ {t('semantic')}
            </button>
          </div>

          {search.status === 'success' && (
            <div className="flex items-center justify-between">
              <div className="text-xs text-gray-500 dark:text-gray-400">
                {t('results_count', { total: search.total })} ({search.tookMs}ms) — {t('page_of', { page: search.page, total: totalPages })}
              </div>
              <div className="flex items-center gap-2">
                <select
                  value={search.sortField}
                  onChange={e => search.setSort(e.target.value)}
                  className="text-xs px-2 py-1 border border-gray-200 dark:border-gray-700 rounded bg-gray-50 dark:bg-gray-800 text-gray-600 dark:text-gray-400"
                >
                  <option value="score">{t('by_relevance')}</option>
                  <option value="date">{t('by_date')}</option>
                  <option value="name">{t('by_name')}</option>
                  <option value="size">{t('by_size')}</option>
                </select>
                {exportMsg && (
                  <span className="text-xs text-gray-500 dark:text-gray-400 max-w-48 truncate">{exportMsg}</span>
                )}
                <button
                  onClick={handleExport}
                  disabled={false}
                  className="px-2 py-1 text-xs font-medium text-gray-500 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors shrink-0 disabled:opacity-40 disabled:cursor-not-allowed"
                >
                  {t('export_csv')}
                </button>
              </div>
            </div>
          )}
        </div>

        <div className="flex-1 overflow-y-auto">
          {search.status === 'idle' && (
            <EmptyState
              icon={<SearchIcon className="size-12" />}
              title={t('search_your_documents')}
              description={t('search_description')}
            />
          )}

          {search.status === 'loading' && (
            <ResultListSkeleton />
          )}

          {search.status === 'error' && (
            <div className="flex flex-col items-center justify-center py-16 px-4">
              <p className="text-sm text-red-600 dark:text-red-400 mb-3">{search.error}</p>
              <button
                onClick={search.retry}
                className="px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors"
              >
                {t('retry')}
              </button>
            </div>
          )}

          {search.status === 'success' && (
            <ResultList
              hits={search.hits}
              selectedId={selectedHit?.file_id ?? null}
              onSelect={setSelectedHit}
            />
          )}

          {search.status === 'success' && search.hits.length === 0 && (
            <div className="flex flex-col items-center justify-center py-16 px-4 text-center space-y-3">
              <p className="text-lg font-medium text-gray-900 dark:text-gray-100">{t('no_results_found')}</p>
              <p className="text-sm text-gray-500 dark:text-gray-400">
                {t('no_results_hint')}
              </p>
              <button
                onClick={() => {
                  search.setExtFilter([])
                  search.setDirIds([])
                  search.setDirPaths([])
                }}
                className="px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors whitespace-nowrap"
              >
                {t('clear_filters')}
              </button>
              <p className="text-xs text-gray-500 dark:text-gray-400">
                📊 {t('index_status')}:
                <Link
                  to="/index"
                  className="text-blue-600 dark:text-blue-500 underline hover:text-blue-700 ml-1"
                >
                  {t('index_status')}
                </Link>
              </p>
            </div>
          )}
        </div>

        {search.status === 'success' && totalPages > 1 && (
          <div className="flex items-center justify-center gap-2 px-4 py-3 border-t border-gray-100 dark:border-gray-800">
            <button
              onClick={() => search.setPage(search.page - 1)}
              disabled={search.page <= 1}
              className="px-3 py-1 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
            >
              {t('prev_page')}
            </button>
            
            <div className="flex items-center gap-2">
              <span className="text-xs text-gray-500 dark:text-gray-400">{t('go_to')}</span>
              <input
                type="number"
                value={search.page}
                onChange={(e) => {
                  const page = parseInt(e.target.value, 10)
                  if (!isNaN(page) && page >= 1 && page <= totalPages) {
                    search.setPage(page)
                  }
                }}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault()
                    const input = e.target as HTMLInputElement
                    const page = parseInt(input.value, 10)
                    if (!isNaN(page) && page >= 1 && page <= totalPages) {
                      search.setPage(page)
                      input.blur()
                    }
                  }
                }}
                className="w-20 px-2 py-1 text-xs border border-gray-200 dark:border-gray-700 rounded bg-gray-50 dark:bg-gray-800 text-gray-600 dark:text-gray-400 text-center focus:outline-none focus:ring-2 focus:ring-blue-500"
                min={1}
                max={totalPages}
              />
            </div>
            
            <button
              onClick={() => search.setPage(search.page + 1)}
              disabled={search.page >= totalPages}
              className="px-3 py-1 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
            >
              {t('next_page')}
            </button>
          </div>
        )}
      </div>

      <PreviewPanel
        fileId={selectedHit?.file_id ?? null}
        searchQuery={search.query}
        onClose={() => setSelectedHit(null)}
      />
    </div>
  )
}
