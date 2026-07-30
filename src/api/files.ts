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