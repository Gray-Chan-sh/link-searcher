import { invoke } from '@tauri-apps/api/core'

export interface DirConfig {
  id: string
  path: string
  alias: string | null
  ocr_lang: string
  exclude_patterns: string | null
  include_exts: string | null
  recursive: boolean
  total_files?: number
  indexed_files?: number
}

export async function addDir(path: string, alias?: string, recursive?: boolean): Promise<DirConfig> {
  return invoke<DirConfig>('add_dir', { path, alias, recursive })
}

export async function removeDir(id: string): Promise<void> {
  return invoke('remove_dir', { id })
}

export async function listDirs(): Promise<DirConfig[]> {
  return invoke<DirConfig[]>('list_dirs')
}

export interface DirTreeNode {
  name: string
  path: string
  is_dir: boolean
  children: DirTreeNode[]
}

export async function getDirTree(dirId: string, includeFiles?: boolean): Promise<DirTreeNode> {
  return invoke<DirTreeNode>('get_dir_tree', { dirId, includeFiles: includeFiles ?? false })
}

export async function getDirChildren(parentPath: string): Promise<DirTreeNode[]> {
  return invoke<DirTreeNode[]>('get_dir_children', { parentPath })
}