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
  /** 是否参与扫描与文件监控；false = 已停用（保留数据）。 */
  monitor_enabled: boolean
  total_files?: number
  indexed_files?: number
}

export async function addDir(path: string, alias?: string, recursive?: boolean): Promise<DirConfig> {
  return client.invoke<DirConfig>('add_dir', { path, alias, recursive })
}

/** 停用 / 启用某个监控目录（停用 = 暂停扫描与监控，但保留 dir_config 行与数据）。 */
export async function setDirEnabled(id: string, enabled: boolean): Promise<void> {
  return client.invoke('update_dir', { id, monitorEnabled: enabled })
}

/** Report returned by the `remove_dir` command. */
export interface RemoveDirReport {
  message: string
  stopped_scan: boolean
  swept_orphan_dirs: number
  resumed_scan: boolean
}

/** Remove a monitored directory. Returns a human-readable summary (the dialog
 *  stays open until the backend has stopped scanning, purged the data, swept
 *  leftovers and resumed indexing the remaining directories).
 *
 *  NOTE: in Tauri mode `client.invoke` returns the raw command result (the
 *  `transform` below only applies to the HTTP transport), so normalise both
 *  shapes here — rendering the report object directly crashes React (#31). */
export async function removeDir(id: string, purge = false): Promise<string> {
  const res = await client.invoke<RemoveDirReport | string>('remove_dir', { id, purge })
  if (typeof res === 'string') return res
  return res?.message ?? ''
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
