import * as client from './client'

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

export interface IdWithPath {
  file_id: string
  path: string
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
  return client.invoke<SearchResponse>('search', {
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
  return client.invoke<string[]>('suggest', { prefix })
}

export async function getSearchHistory(): Promise<SearchHistoryEntry[]> {
  return client.invoke<SearchHistoryEntry[]>('get_search_history')
}

export async function clearSearchHistory(): Promise<void> {
  return client.invoke<void>('clear_search_history')
}

export async function exportSearchResults(
  query: string,
  dirIds?: string[],
  extFilter?: string[],
  format?: string,
): Promise<string> {
  return client.invoke<string>('export_search_results', {
    query,
    dirIds: dirIds ?? [],
    extFilter: extFilter ?? [],
    format: format ?? 'csv',
  })
}

export async function getFileTypeStats(): Promise<FileTypeStat[]> {
  return client.invoke<FileTypeStat[]>('get_file_type_stats')
}

export async function searchFileIdsOnly(
  query: string,
  dirIds?: string[],
  dirPaths?: string[],
  extFilter?: string[],
  semantic?: boolean,
): Promise<IdWithPath[]> {
  return client.invoke<IdWithPath[]>('search_file_ids_only', {
    query,
    dirIds: dirIds ?? [],
    dirPaths: dirPaths ?? [],
    extFilter: extFilter ?? [],
    semantic: semantic ?? false,
  })
}

export interface RefineSearchResponse {
  total: number
  hits: SearchHit[]
  took_ms: number
}

export async function refineSearch(
  query: string,
  fileIds: string[],
  page?: number,
  pageSize?: number,
): Promise<RefineSearchResponse> {
  return client.invoke<RefineSearchResponse>('refine_search', {
    query,
    fileIds,
    page: page ?? 1,
    pageSize: pageSize ?? 200,
  })
}
