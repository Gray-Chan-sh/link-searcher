import { useCallback, useEffect, useRef, useState } from 'react'
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
  const [dirIds, setDirIds] = useState<string[]>([])
  const [dirPaths, setDirPaths] = useState<string[]>([])
  const [extFilter, setExtFilter] = useState<string[]>([])
  const [suggestions, setSuggestions] = useState<string[]>([])
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const abortRef = useRef<AbortController | null>(null)

  const executeSearch = useCallback(async (
    q: string,
    p: number,
    ps: number,
    dirs: string[],
    exts: string[],
    paths: string[],
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
      const res: SearchResponse = await search(q, p, ps, dirs, paths, exts)
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

  const setQuery = useCallback((q: string) => {
    setState(s => ({ ...s, query: q }))
    executeSearch(q, 1, state.pageSize, dirIds, extFilter, dirPaths)
  }, [executeSearch, state.pageSize, dirIds, extFilter, dirPaths])

  const setPage = useCallback((p: number) => {
    executeSearch(state.query, p, state.pageSize, dirIds, extFilter, dirPaths)
  }, [executeSearch, state.query, state.pageSize, dirIds, extFilter, dirPaths])

  const updateDirIds = useCallback((ids: string[]) => {
    setDirIds(ids)
    executeSearch(state.query, 1, state.pageSize, ids, extFilter, dirPaths)
  }, [executeSearch, state.query, state.pageSize, extFilter, dirPaths])

  const updateDirPaths = useCallback((paths: string[]) => {
    setDirPaths(paths)
    executeSearch(state.query, 1, state.pageSize, dirIds, extFilter, paths)
  }, [executeSearch, state.query, state.pageSize, dirIds, extFilter])

  const updateExtFilter = useCallback((exts: string[]) => {
    setExtFilter(exts)
    executeSearch(state.query, 1, state.pageSize, dirIds, exts, dirPaths)
  }, [executeSearch, state.query, state.pageSize, dirIds, extFilter, dirPaths])

  const retry = useCallback(() => {
    executeSearch(state.query, state.page, state.pageSize, dirIds, extFilter, dirPaths)
  }, [executeSearch, state.query, state.page, state.pageSize, dirIds, extFilter, dirPaths])

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
    suggestions,
    setQuery,
    setPage,
    setDirIds: updateDirIds,
    setDirPaths: updateDirPaths,
    setExtFilter: updateExtFilter,
    retry,
    fetchSuggestions,
    clearSuggestions: () => setSuggestions([]),
  }
}
