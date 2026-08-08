import { invoke } from '@tauri-apps/api/core'

export interface FileDetail {
  id: string
  path: string
  file_name: string
  file_ext: string
  mtime: number
  file_size: number
  md5: string | null
  indexed: boolean
}

export interface DuplicateGroup {
  md5: string
  count: number
  paths: string[]
}

export interface SummaryResult {
  file_id: string
  summary: string
  cached: boolean
}

export async function summarizeFile(fileId: string): Promise<SummaryResult> {
  return invoke<SummaryResult>('summarize_file', { fileId })
}

export async function askDocuments(fileIds: string[], question: string): Promise<string> {
  return invoke<string>('ask_documents', { fileIds, question })
}

export async function getFile(id: string): Promise<FileDetail> {
  return invoke<FileDetail>('get_file', { id })
}

export async function getDuplicates(): Promise<DuplicateGroup[]> {
  return invoke<DuplicateGroup[]>('get_duplicates')
}

export async function previewFile(id: string): Promise<string> {
  return invoke<string>('preview_file', { id })
}

export interface FilePreview {
  content: string | null
  image_path: string | null
  file_type: string  // 'image' | 'text' | 'pdf' | 'office' | 'unknown'
  char_count: number
  ocr_used: boolean
}

export async function getFilePreview(id: string): Promise<FilePreview> {
  return invoke<FilePreview>('get_file_preview', { id })
}

export async function revealInFolder(id: string): Promise<void> {
  return invoke('reveal_in_folder', { id })
}

export async function openFile(id: string): Promise<void> {
  return invoke('open_file', { id })
}

export interface FileItem {
  file_id: string
  file_name: string
  rel_path: string
  file_ext: string
  indexed: number  // 0=pending, 1=indexed, 2=failed
  error_msg: string | null
  file_size: number
  mtime: number
}

export interface FileListResponse {
  items: FileItem[]
  total: number
  page: number
  page_size: number
}

export type FilterType = 'all' | 'indexed' | 'pending' | 'failed'
export type SortKey = 'name' | 'size' | 'mtime' | 'ext'
export type SortOrder = 'asc' | 'desc'

export async function listFilesDb(params: {
  filter?: FilterType
  ext?: string
  search?: string
  sort?: SortKey
  order?: SortOrder
  page?: number
  pageSize?: number
}): Promise<FileListResponse> {
  return invoke<FileListResponse>('list_files_db', params)
}

export async function getBrowseFileTypes(): Promise<string[]> {
  return invoke<string[]>('get_browse_file_types')
}