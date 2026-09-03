import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
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
import { exportSearchResults, searchFileIdsOnly, refineSearch, type IdWithPath } from '../api/search'
import { openFile, aiCapabilities, type AiCapabilities } from '../api/files'
import { SearchIcon } from '../icons'
import { exportFile } from '../utils/platform'

interface RefineStep {
  hits: SearchHit[]
  selectedIds: Set<string>
  query: string
  tookMs: number
}

export default function SearchPage() {
  const { t } = useI18n()
  const navigate = useNavigate()
  const search = useSearch()
  const { dirs } = useDirs()
  const [selectedHit, setSelectedHit] = useState<SearchHit | null>(null)
  const [focusIndex, setFocusIndex] = useState(-1)
  const [showFilters, setShowFilters] = useState(true)
  const [filterWidth, setFilterWidth] = useState(224)
  const [exportMsg, setExportMsg] = useState<string | null>(null)
  const [aiCap, setAiCap] = useState<AiCapabilities>({ embedding: false, llm: false })
  const timersRef = useRef<ReturnType<typeof setTimeout>[]>([])

  const [refineHistory, setRefineHistory] = useState<RefineStep[]>(() => {
    const s = sessionStorage.getItem('ls_search_refine')
    if (s) { try {
      const d = JSON.parse(s)
      return (d.history ?? []).map((step: Record<string, unknown>) => ({
        ...step,
        selectedIds: new Set<string>(Array.isArray(step.selectedIds) ? step.selectedIds : []),
      }))
    } catch {} }
    return []
  })
  const [refineIndex, setRefineIndex] = useState(() => {
    const s = sessionStorage.getItem('ls_search_refine')
    if (s) { try { const d = JSON.parse(s); return d.index ?? 0 } catch {} }
    return 0
  })
  const [refineQuery, setRefineQuery] = useState('')
  const [refineLoading, setRefineLoading] = useState(false)
  const [allFileRows, setAllFileRows] = useState<IdWithPath[]>(() => {
    const s = sessionStorage.getItem('ls_search_refine')
    if (s) { try { const d = JSON.parse(s); return d.allFileRows ?? [] } catch {} }
    return []
  })
  const refineInputRef = useRef<HTMLInputElement>(null)

  const isRefining = refineHistory.length > 0
  const currentRefine = isRefining ? refineHistory[refineIndex] : null

  useEffect(() => {
    sessionStorage.setItem('ls_search_refine', JSON.stringify({
      history: refineHistory, index: refineIndex, allFileRows,
    }))
  }, [refineHistory, refineIndex, allFileRows])

  useEffect(() => { aiCapabilities().then(setAiCap).catch(() => {}) }, [])
  useEffect(() => () => timersRef.current.forEach(clearTimeout), [])

  const totalPages = useMemo(
    () => Math.max(1, Math.ceil(search.total / search.pageSize)),
    [search.total, search.pageSize],
  )

  useEffect(() => {
    if (search.status === 'success') {
      setRefineHistory([])
      setRefineIndex(0)
      setRefineQuery('')
      setAllFileRows([])
    }
  }, [search.status, search.query])

  const handleExtToggle = (ext: string) => {
    const next = search.extFilter.includes(ext)
      ? search.extFilter.filter(e => e !== ext)
      : [...search.extFilter, ext]
    search.setExtFilter(next)
  }

  const handleExport = async () => {
    try {
      const content = await exportSearchResults(search.query, search.dirIds, search.extFilter, 'csv')
      await exportFile(content, 'results.csv', 'text/csv')
    } catch (e) {
      setExportMsg(t('export_failed', { error: e instanceof Error ? e.message : t('unknown_error') }))
      timersRef.current.push(setTimeout(() => setExportMsg(null), 5000))
    }
  }

  const enterRefine = useCallback(async () => {
    if (search.hits.length === 0) return
    setRefineLoading(true)
    try {
      const rows = await searchFileIdsOnly(
        search.query, search.dirIds, search.dirPaths, search.extFilter, search.semantic,
      )
      setAllFileRows(rows)
      setRefineHistory([{
        hits: search.hits,
        selectedIds: new Set(rows.map(r => r.file_id)),
        query: '',
        tookMs: search.tookMs,
      }])
      setRefineIndex(0)
      setRefineQuery('')
    } catch {
    } finally {
      setRefineLoading(false)
    }
  }, [search])

  const exitRefine = () => {
    setRefineHistory([])
    setRefineIndex(0)
    setAllFileRows([])
    setRefineQuery('')
  }

  const doRefine = useCallback(async (q: string) => {
    if (!currentRefine || currentRefine.selectedIds.size === 0) return
    setRefineLoading(true)
    try {
      const res = await refineSearch(q, Array.from(currentRefine.selectedIds))
      setRefineHistory(prev => [...prev.slice(0, refineIndex + 1), {
        hits: res.hits,
        selectedIds: new Set(res.hits.map(h => h.file_id)),
        query: q,
        tookMs: res.took_ms,
      }])
      setRefineIndex((prev: number) => prev + 1)
      setRefineQuery('')
    } catch {
    } finally {
      setRefineLoading(false)
    }
  }, [currentRefine, refineIndex])

  const handleRefineSubmit = () => { const q = refineQuery.trim(); if (q) doRefine(q) }
  const handleRefineKeyDown = (e: React.KeyboardEvent) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleRefineSubmit() } }
  const goBack = () => { if (refineIndex > 0) setRefineIndex((prev: number) => prev - 1) }
  const goForward = () => { if (refineIndex < refineHistory.length - 1) setRefineIndex((prev: number) => prev + 1) }
  const resetRefine = () => { setRefineIndex(0); setRefineQuery('') }

  const toggleRefineFile = (fileId: string) => {
    if (!currentRefine) return
    const next = new Set(currentRefine.selectedIds)
    if (next.has(fileId)) next.delete(fileId); else next.add(fileId)
    setRefineHistory(prev => { const c = [...prev]; c[refineIndex] = { ...c[refineIndex]!, selectedIds: next }; return c })
  }

  const toggleRefineSelectAll = () => {
    if (!currentRefine) return
    const all = currentRefine.selectedIds.size === currentRefine.hits.length
    const next = all ? new Set<string>() : new Set(currentRefine.hits.map(h => h.file_id))
    setRefineHistory(prev => { const c = [...prev]; c[refineIndex] = { ...c[refineIndex]!, selectedIds: next }; return c })
  }

  const goToChat = () => {
    if (!currentRefine || currentRefine.selectedIds.size === 0) return
    const byId = new Map(allFileRows.map(r => [r.file_id, r.path]))
    const paths: string[] = []
    for (const id of currentRefine.selectedIds) {
      const p = byId.get(id)
      if (p) paths.push(p)
    }
    sessionStorage.setItem('ls_pending_chat_paths', JSON.stringify(paths))
    sessionStorage.setItem('ls_pending_chat_query', search.query)
    navigate('/chat')
  }

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const active = document.activeElement
      if (active?.closest('[data-search-input]') || active?.closest('[data-refine-input]')) return
      const hits = isRefining && currentRefine ? currentRefine.hits : search.hits
      if (search.status !== 'success' || hits.length === 0) return
      if (e.key === 'ArrowDown') { e.preventDefault(); const n = focusIndex + 1; if (n < hits.length) { setFocusIndex(n); setSelectedHit(hits[n]!) } }
      else if (e.key === 'ArrowUp') { e.preventDefault(); const p = focusIndex - 1; if (p >= 0) { setFocusIndex(p); setSelectedHit(hits[p]!) } }
      else if (e.key === 'Enter' && focusIndex >= 0) { e.preventDefault(); openFile(hits[focusIndex]!.file_id) }
    }
    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [search.status, search.hits, isRefining, currentRefine, focusIndex])

  useEffect(() => { setFocusIndex(-1) }, [search.hits, refineIndex])

  const displayHits = isRefining && currentRefine ? currentRefine.hits : search.hits

  return (
    <div className="flex h-full">
      {showFilters && (
        <FilterPanel dirs={dirs} dirPaths={search.dirPaths} extFilter={search.extFilter}
          onDirPathsChange={search.setDirPaths} onExtToggle={handleExtToggle}
          onClearFilters={() => { search.setDirIds([]); search.setDirPaths([]); search.setExtFilter([]) }}
          width={filterWidth} onWidthChange={setFilterWidth} />
      )}

      <div className="flex-1 flex flex-col min-w-0">
        <div className="px-4 pt-4 pb-2 space-y-3">
          <div className="flex items-center gap-2">
            <div className="flex-1">
              <SearchBar query={search.query} loading={search.status === 'loading'}
                suggestions={search.suggestions}
                onQueryChange={search.setQuery} onSubmit={search.submitSearch}
                onFetchSuggestions={search.fetchSuggestions}
                onClearSuggestions={search.clearSuggestions}
                onPickSuggestion={q => { search.setQuery(q) }} />
            </div>
            <button onClick={() => setShowFilters(v => !v)}
              className="px-2.5 py-2 text-xs font-medium text-gray-500 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors shrink-0">{t('filters')}</button>
            <button onClick={() => search.setSemantic(!search.semantic)} disabled={!aiCap.embedding}
              title={aiCap.embedding ? t('semantic_search') : t('ai_embedding_unavailable')}
              className={`px-2.5 py-2 text-xs font-medium rounded-lg border transition-colors shrink-0 ${
                !aiCap.embedding ? 'text-gray-300 dark:text-gray-600 cursor-not-allowed'
                  : search.semantic ? 'text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/20 border-blue-300 dark:border-blue-800'
                  : 'text-gray-500 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border-gray-200 dark:border-gray-700 hover:bg-gray-100 dark:hover:bg-gray-700'
              }`}>✦ {t('semantic')}</button>
          </div>

          {search.status === 'success' && (
            <div className="flex items-center justify-between">
              <div className="text-xs text-gray-500 dark:text-gray-400">
                {isRefining ? (
                  <span>
                    🔍 {allFileRows.length} 篇文档
                    {refineHistory.length > 1 && <span className="ml-1">({refineHistory.slice(0, refineIndex + 1).map(s => s.hits.length).join(' → ')})</span>}
                    {currentRefine?.query && <span className="ml-2 text-gray-400">「{currentRefine.query}」</span>}
                    <span className="ml-2 text-gray-400">第 {refineIndex + 1}/{refineHistory.length} 步</span>
                  </span>
                ) : (
                  <span>{t('results_count', { total: search.total })} ({search.tookMs}ms) — {t('page_of', { page: search.page, total: totalPages })}</span>
                )}
              </div>
              <div className="flex items-center gap-2">
                {isRefining ? (
                  <>
                    <button onClick={goBack} disabled={refineIndex <= 0}
                      className="px-2 py-1 text-xs rounded border border-gray-200 dark:border-gray-700 text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 disabled:opacity-30 disabled:cursor-not-allowed transition-colors">←</button>
                    <button onClick={goForward} disabled={refineIndex >= refineHistory.length - 1}
                      className="px-2 py-1 text-xs rounded border border-gray-200 dark:border-gray-700 text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 disabled:opacity-30 disabled:cursor-not-allowed transition-colors">→</button>
                    <button onClick={resetRefine} disabled={refineIndex === 0}
                      className="px-2 py-1 text-xs rounded border border-gray-200 dark:border-gray-700 text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 disabled:opacity-30 transition-colors">清空</button>
                    <button onClick={exitRefine}
                      className="px-2 py-1 text-xs rounded border border-gray-200 dark:border-gray-700 text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors">退出</button>
                    <button onClick={goToChat} disabled={!currentRefine || currentRefine.selectedIds.size === 0}
                      className="px-3 py-1 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-40 rounded-lg transition-colors">
                      去聊天 ({currentRefine?.selectedIds.size ?? 0})
                    </button>
                  </>
                ) : (
                  <>
                    {search.hits.length > 0 && (
                      <button onClick={enterRefine} disabled={refineLoading}
                        className="px-2.5 py-1 text-xs font-medium rounded-md border transition-colors shrink-0 text-gray-500 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border-gray-200 dark:border-gray-700 hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-50">
                        {refineLoading ? '…' : '🔍 缩小范围'}
                      </button>
                    )}
                    <select value={search.sortField} onChange={e => search.setSort(e.target.value)}
                      className="text-xs px-2 py-1 border border-gray-200 dark:border-gray-700 rounded bg-gray-50 dark:bg-gray-800 text-gray-600 dark:text-gray-400">
                      <option value="score">{t('by_relevance')}</option>
                      <option value="date">{t('by_date')}</option>
                      <option value="name">{t('by_name')}</option>
                      <option value="size">{t('by_size')}</option>
                    </select>
                    {exportMsg && <span className="text-xs text-gray-500 dark:text-gray-400 max-w-48 truncate">{exportMsg}</span>}
                    <button onClick={handleExport}
                      className="px-2 py-1 text-xs font-medium text-gray-500 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors shrink-0">{t('export_csv')}</button>
                  </>
                )}
              </div>
            </div>
          )}

          {isRefining && (
            <div className="flex items-center gap-2">
              <input ref={refineInputRef} data-refine-input value={refineQuery} onChange={e => setRefineQuery(e.target.value)}
                onKeyDown={handleRefineKeyDown} placeholder="在结果内再搜索…"
                className="flex-1 px-3 py-1.5 text-sm rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800 text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500/40" />
              <button onClick={handleRefineSubmit} disabled={!refineQuery.trim() || refineLoading}
                className="px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-40 rounded-lg transition-colors">
                {refineLoading ? '…' : '搜索'}
              </button>
              {currentRefine && currentRefine.hits.length > 0 && (
                <button onClick={toggleRefineSelectAll}
                  className="text-xs text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 transition-colors whitespace-nowrap">
                  {currentRefine.selectedIds.size === currentRefine.hits.length ? t('deselect_all_refine') : t('select_all_refine')}
                </button>
              )}
            </div>
          )}
        </div>

        <div className="flex-1 overflow-y-auto">
          {search.status === 'idle' && !isRefining && (
            <EmptyState icon={<SearchIcon className="size-12" />} title={t('search_your_documents')} description={t('search_description')} />
          )}
          {search.status === 'loading' && <ResultListSkeleton />}
          {search.status === 'error' && (
            <div className="flex flex-col items-center justify-center py-16 px-4">
              <p className="text-sm text-red-600 dark:text-red-400 mb-3">{search.error}</p>
              <button onClick={search.retry} className="px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors">{t('retry')}</button>
            </div>
          )}
          {displayHits.length > 0 && (
            <ResultList hits={displayHits} selectedId={selectedHit?.file_id ?? null} onSelect={setSelectedHit}
              revealIndex={focusIndex}
              checkboxIds={isRefining && currentRefine ? currentRefine.selectedIds : undefined}
              onCheckboxToggle={isRefining ? toggleRefineFile : undefined} />
          )}
          {search.status === 'success' && !isRefining && displayHits.length === 0 && (
            <div className="flex flex-col items-center justify-center py-16 px-4 text-center space-y-3">
              <p className="text-lg font-medium text-gray-900 dark:text-gray-100">{t('no_results_found')}</p>
              <p className="text-sm text-gray-500 dark:text-gray-400">{t('no_results_hint')}</p>
              <button onClick={() => { search.setExtFilter([]); search.setDirIds([]); search.setDirPaths([]) }}
                className="px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors whitespace-nowrap">{t('clear_filters')}</button>
              <p className="text-xs text-gray-500 dark:text-gray-400">📊 {t('index_status')}: <Link to="/index" className="text-blue-600 dark:text-blue-500 underline hover:text-blue-700 ml-1">{t('index_status')}</Link></p>
            </div>
          )}
          {isRefining && displayHits.length === 0 && !refineLoading && (
            <div className="flex items-center justify-center h-full text-sm text-gray-400 dark:text-gray-500 select-none px-4 text-center">{t('refine_empty')}</div>
          )}
        </div>

        {!isRefining && search.status === 'success' && totalPages > 1 && (
          <div className="flex items-center justify-center gap-2 px-4 py-3 border-t border-gray-100 dark:border-gray-800">
            <button onClick={() => search.setPage(search.page - 1)} disabled={search.page <= 1}
              className="px-3 py-1 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed transition-colors">{t('prev_page')}</button>
            <div className="flex items-center gap-2">
              <span className="text-xs text-gray-500 dark:text-gray-400">{t('go_to')}</span>
              <input type="number" value={search.page}
                onChange={e => { const p = parseInt(e.target.value, 10); if (!isNaN(p) && p >= 1 && p <= totalPages) search.setPage(p) }}
                onKeyDown={e => { if (e.key === 'Enter') { e.preventDefault(); const i = e.target as HTMLInputElement; const p = parseInt(i.value, 10); if (!isNaN(p) && p >= 1 && p <= totalPages) { search.setPage(p); i.blur() } } }}
                className="w-20 px-2 py-1 text-xs border border-gray-200 dark:border-gray-700 rounded bg-gray-50 dark:bg-gray-800 text-gray-600 dark:text-gray-400 text-center focus:outline-none focus:ring-2 focus:ring-blue-500" min={1} max={totalPages} />
            </div>
            <button onClick={() => search.setPage(search.page + 1)} disabled={search.page >= totalPages}
              className="px-3 py-1 text-xs font-medium text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed transition-colors">{t('next_page')}</button>
          </div>
        )}
      </div>

      <PreviewPanel fileId={selectedHit?.file_id ?? null} searchQuery={search.query} onClose={() => setSelectedHit(null)} />
    </div>
  )
}
