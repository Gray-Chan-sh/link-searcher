import { invoke } from '@tauri-apps/api/core'

export interface SearchHit {
  file_id: string
  file_name: string
  file_ext: string
  path: string
  snippet: string
  score: number
  mtime: number
  file_size: number
}

export interface SearchResponse {
  total: number
  page: number
  page_size: number
  took_ms: number
  hits: SearchHit[]
}

export interface SearchHistoryEntry {
  id: string
  query: string
  dir_ids: string | null
  filters: string | null
  result_count: number
  pinned: boolean
  created_at: number
}

export interface FileTypeStat {
  extension: string
  name: string
  count: number
  indexed: number
  pending: number
  failed: number
}

export async function search(
  query: string,
  page: number,
  pageSize: number,
  dirIds?: string[],
  dirPaths?: string[],
  extFilter?: string[],
  sort?: string,
  sortOrder?: string,
  semantic?: boolean,
): Promise<SearchResponse> {
  return invoke<SearchResponse>('search', {
    query,
    page,
    pageSize,
    dirIds: dirIds ?? [],
    dirPaths: dirPaths ?? [],
    extFilter: extFilter ?? [],
    sort,
    sortOrder,
    semantic: semantic ?? false,
  })
}

export async function suggest(prefix: string): Promise<string[]> {
  return invoke<string[]>('suggest', { prefix })
}

export async function getSearchHistory(): Promise<SearchHistoryEntry[]> {
  return invoke<SearchHistoryEntry[]>('get_search_history')
}

export async function clearSearchHistory(): Promise<void> {
  return invoke<void>('clear_search_history')
}

export async function exportSearchResults(
  query: string,
  dirIds?: string[],
  extFilter?: string[],
  format?: string,
): Promise<string> {
  return invoke<string>('export_search_results', {
    query,
    dirIds: dirIds ?? [],
    extFilter: extFilter ?? [],
    format: format ?? 'csv',
  })
}

export async function getFileTypeStats(): Promise<FileTypeStat[]> {
  return invoke<FileTypeStat[]>('get_file_type_stats')
}
