import * as client from './client'
import { normalizePath } from '../utils/normalizePath'

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

/** Remove a monitored directory. Returns a human-readable summary (the dialog
 *  stays open until the backend has stopped scanning, purged the data, swept
 *  leftovers and resumed indexing the remaining directories). */
export async function removeDir(id: string, purge = false): Promise<string> {
  return client.invoke<string>('remove_dir', { id, purge })
}

function normNode(n: DirTreeNode): DirTreeNode {
  return { ...n, path: normalizePath(n.path), children: n.children.map(normNode) }
}

export async function listDirs(): Promise<DirConfig[]> {
  const dirs = await client.invoke<DirConfig[]>('list_dirs')
  return dirs.map(d => ({ ...d, path: normalizePath(d.path) }))
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
  return normNode(await client.invoke<DirTreeNode>('get_dir_tree', { dirId, includeFiles: includeFiles ?? false }))
}

export async function getDirChildren(parentPath: string): Promise<DirTreeNode[]> {
  const items = await client.invoke<DirTreeNode[]>('get_dir_children', { parentPath })
  return items.map(normNode)
}

export async function updateDir(id: string, alias?: string, ocrLang?: string, excludePatterns?: string, includeExts?: string, recursive?: boolean, private_?: boolean): Promise<void> {
  return client.invoke<void>('update_dir', { id, alias, ocrLang, excludePatterns, includeExts, recursive, private: private_ })
}