import * as client from './client'

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
  private?: boolean
}

export async function addDir(path: string, alias?: string, recursive?: boolean): Promise<DirConfig> {
  return client.invoke<DirConfig>('add_dir', { path, alias, recursive })
}

export async function removeDir(id: string): Promise<void> {
  return client.invoke('remove_dir', { id })
}

export async function listDirs(): Promise<DirConfig[]> {
  return client.invoke<DirConfig[]>('list_dirs')
}

export interface DirTreeNode {
  name: string
  path: string
  is_dir: boolean
  children: DirTreeNode[]
  indexed?: boolean
  status?: string
}

export async function getDirTree(dirId: string, includeFiles?: boolean): Promise<DirTreeNode> {
  return client.invoke<DirTreeNode>('get_dir_tree', { dirId, includeFiles: includeFiles ?? false })
}

export async function getDirChildren(parentPath: string): Promise<DirTreeNode[]> {
  return client.invoke<DirTreeNode[]>('get_dir_children', { parentPath })
}

export async function updateDir(id: string, alias?: string, ocrLang?: string, excludePatterns?: string, includeExts?: string, recursive?: boolean, private_?: boolean): Promise<void> {
  return client.invoke<void>('update_dir', { id, alias, ocrLang, excludePatterns, includeExts, recursive, private: private_ })
}