import { useCallback, useEffect, useRef, useState, useDeferredValue } from 'react'
import { search, suggest, type SearchHit, type SearchResponse } from '../api/search'

interface SearchState {
  status: 'idle' | 'loading' | 'success' | 'error'
  query: string
  page: number
  pageSize: number
  total: number
  tookMs: number
  hits: SearchHit[]
  error: string | null
}

const DEFAULT_PAGE_SIZE = 20

export function useSearch() {
  const [state, setState] = useState<SearchState>({
    status: 'idle',
    query: '',
    page: 1,
    pageSize: DEFAULT_PAGE_SIZE,
    total: 0,
    tookMs: 0,
    hits: [],
    error: null,
  })
  const [sortField, setSortField] = useState<string>('score')
  const [sortOrder, setSortOrder] = useState<string>('desc')
  const [dirIds, setDirIds] = useState<string[]>([])
  const [dirPaths, setDirPaths] = useState<string[]>([])
  const [extFilter, setExtFilter] = useState<string[]>([])
  const [suggestions, setSuggestions] = useState<string[]>([])
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const abortRef = useRef<AbortController | null>(null)

  // Deferred query for useDeferredValue-based debounce
  const deferredQuery = useDeferredValue(state.query)

  // Filter ref to always hold the latest values (avoids closure staleness)
  const filtersRef = useRef<{
    pageSize: number
    dirIds: string[]
    extFilter: string[]
    dirPaths: string[]
    sortField: string
    sortOrder: string
  }>({
    pageSize: state.pageSize,
    dirIds,
    extFilter,
    dirPaths,
    sortField,
    sortOrder,
  })

  // Keep filtersRef in sync with state and filter states
  useEffect(() => {
    filtersRef.current = {
      pageSize: state.pageSize,
      dirIds,
      extFilter,
      dirPaths,
      sortField,
      sortOrder,
    }
  }, [state.pageSize, dirIds, extFilter, dirPaths, sortField, sortOrder])

  const executeSearch = useCallback(async (
    q: string,
    p: number,
    ps: number,
    dirs: string[],
    exts: string[],
    paths: string[],
    sort?: string,
    sortOrder?: string,
  ) => {
    abortRef.current?.abort()
    const ctrl = new AbortController()
    abortRef.current = ctrl

    if (!q.trim()) {
      setState(s => ({ ...s, status: 'idle', query: q, page: 1, hits: [], total: 0 }))
      return
    }

    setState(s => ({ ...s, status: 'loading', query: q, page: p, error: null }))

    try {
      const res: SearchResponse = await search(q, p, ps, dirs, paths, exts, sort, sortOrder)
      if (!ctrl.signal.aborted) {
        setState(s => ({
          ...s,
          status: 'success',
          query: q,
          page: p,
          pageSize: ps,
          total: res.total,
          tookMs: res.took_ms,
          hits: res.hits,
          error: null,
        }))
      }
    } catch (e) {
      if (!ctrl.signal.aborted) {
        setState(s => ({
          ...s,
          status: 'error',
          query: q,
          error: e instanceof Error ? e.message : 'Search failed',
        }))
      }
    }
  }, [])

  // setQuery: only updates query state. Debounced search is triggered separately.
  const setQuery = useCallback((q: string) => {
    setState(s => ({ ...s, query: q }))
  }, [])

  // Immediate search (for Enter key) – uses latest filters from ref
  const submitSearch = useCallback(() => {
    const f = filtersRef.current
    executeSearch(state.query, 1, f.pageSize, f.dirIds, f.extFilter, f.dirPaths, f.sortField, f.sortOrder)
  }, [executeSearch, state.query, filtersRef])

  const setPage = useCallback((p: number) => {
    const f = filtersRef.current
    executeSearch(state.query, p, f.pageSize, f.dirIds, f.extFilter, f.dirPaths, f.sortField, f.sortOrder)
  }, [executeSearch, state.query, filtersRef])

  const updateDirIds = useCallback((ids: string[]) => {
    setDirIds(ids)
    // Update ref synchronously so the subsequent search sees the new ids
    filtersRef.current = { ...filtersRef.current, dirIds: ids }
    const f = filtersRef.current
    executeSearch(state.query, 1, f.pageSize, f.dirIds, f.extFilter, f.dirPaths, f.sortField, f.sortOrder)
  }, [executeSearch, state.query, filtersRef])

  const updateDirPaths = useCallback((paths: string[]) => {
    setDirPaths(paths)
    filtersRef.current = { ...filtersRef.current, dirPaths: paths }
    const f = filtersRef.current
    executeSearch(state.query, 1, f.pageSize, f.dirIds, f.extFilter, f.dirPaths, f.sortField, f.sortOrder)
  }, [executeSearch, state.query, filtersRef])

  const updateExtFilter = useCallback((exts: string[]) => {
    setExtFilter(exts)
    filtersRef.current = { ...filtersRef.current, extFilter: exts }
    const f = filtersRef.current
    executeSearch(state.query, 1, f.pageSize, f.dirIds, f.extFilter, f.dirPaths, f.sortField, f.sortOrder)
  }, [executeSearch, state.query, filtersRef])

  const retry = useCallback(() => {
    const f = filtersRef.current
    executeSearch(state.query, state.page, f.pageSize, f.dirIds, f.extFilter, f.dirPaths, f.sortField, f.sortOrder)
  }, [executeSearch, state.query, state.page, filtersRef])

  const fetchSuggestions = useCallback((prefix: string) => {
    if (debounceRef.current) clearTimeout(debounceRef.current)
    if (!prefix.trim()) {
      setSuggestions([])
      return
    }
    debounceRef.current = setTimeout(async () => {
      try {
        const results = await suggest(prefix)
        setSuggestions(results)
      } catch {
        setSuggestions([])
      }
    }, 200)
  }, [])

  // Debounced search effect: triggers after 300ms of inactivity on the query
  useEffect(() => {
    const timer = setTimeout(() => {
      const f = filtersRef.current
      executeSearch(deferredQuery, 1, f.pageSize, f.dirIds, f.extFilter, f.dirPaths, f.sortField, f.sortOrder)
    }, 300)
    return () => clearTimeout(timer)
  }, [deferredQuery, executeSearch, filtersRef])

  // setSort: update sort field/order and immediately re-execute search with current filters
  const setSort = useCallback((sort: string) => {
    setSortField(sort)
    setSortOrder('desc')
    const f = filtersRef.current
    executeSearch(state.query, 1, f.pageSize, f.dirIds, f.extFilter, f.dirPaths, sort, 'desc')
  }, [executeSearch, state.query, filtersRef])

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current)
      abortRef.current?.abort()
    }
  }, [])

  return {
    ...state,
    dirIds,
    dirPaths,
    extFilter,
    sortField,
    suggestions,
    setQuery,
    setPage,
    setSort,
    setDirIds: updateDirIds,
    setDirPaths: updateDirPaths,
    setExtFilter: updateExtFilter,
    retry,
    fetchSuggestions,
    clearSuggestions: () => setSuggestions([]),
    submitSearch,
  }
}
