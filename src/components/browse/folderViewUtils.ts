/**
 * 文件夹视图（资源管理器风格）的纯逻辑：路径拼接/面包屑/排序/过滤。
 *
 * 抽出来的目的：这些规则容易在不同地方各写一份（路径分隔符、目录优先、
 * Windows 大小写不敏感），单测覆盖后组件只剩渲染。
 */
import type { DirEntry } from '../../api/files'
import { normalizePath } from '../../utils/normalizePath'

/** 统一路径形态：正斜杠、去掉末尾斜杠（保留根 `C:/` 的冒号）。 */
export function cleanPath(p: string): string {
  let out = normalizePath(p.trim())
  while (out.length > 1 && out.endsWith('/') && !out.endsWith(':/')) {
    out = out.slice(0, -1)
  }
  return out
}

/** 父目录；已在根上时返回自身（调用方据此禁用"上一级"）。 */
export function parentPath(p: string): string {
  const clean = cleanPath(p)
  const idx = clean.lastIndexOf('/')
  if (idx <= 0) return clean
  // Windows 盘符根：`C:/` 的上一级仍是 `C:/`
  const parent = clean.slice(0, idx)
  if (/^[a-zA-Z]:$/.test(parent)) return `${parent}/`
  return parent || '/'
}

/** 是否为"这棵根内的同一路径"（Windows 大小写不敏感）。 */
export function isSamePath(a: string, b: string): boolean {
  const norm = (p: string) => {
    const c = cleanPath(p)
    return /^[A-Za-z]:/.test(c) ? c.toLowerCase() : c
  }
  return norm(a) === norm(b)
}

/** `dir` 下拼一个名字（不重复斜杠）。 */
export function joinPath(dir: string, name: string): string {
  const base = cleanPath(dir)
  if (!name) return base
  return base.endsWith('/') ? `${base}${name}` : `${base}/${name}`
}

export interface Breadcrumb {
  label: string
  path: string
}

/**
 * 从资料库根到当前目录的面包屑。
 *
 * 根节点用 `rootLabel`（资料库别名/目录名），其后逐段拼接；
 * `currentPath` 不在 `rootPath` 之下时退化为整段路径显示。
 */
export function breadcrumbs(rootPath: string, rootLabel: string, currentPath: string): Breadcrumb[] {
  const root = cleanPath(rootPath)
  const cur = cleanPath(currentPath)
  const out: Breadcrumb[] = [{ label: rootLabel, path: root }]
  const rootPrefix = root.endsWith('/') ? root : `${root}/`
  if (!cur.toLowerCase().startsWith(rootPrefix.toLowerCase())) {
    return out
  }
  const rest = cur.slice(rootPrefix.length)
  if (!rest) return out
  let acc = root
  for (const part of rest.split('/')) {
    if (!part) continue
    acc = joinPath(acc, part)
    out.push({ label: part, path: acc })
  }
  return out
}

/** 是否已经是绝对路径（Windows 盘符或 POSIX 根）。 */
export function looksAbsolute(p: string): boolean {
  const s = p.trim()
  return /^[A-Za-z]:[\\/]/.test(s) || s.startsWith('/') || s.startsWith('\\\\')
}

/**
 * 深链 `?path=`（库里存的是相对资料库根的路径）→ 绝对路径，**纯本地兜底**。
 *
 * 多资料库时只有 DB 知道归属，正规路径是后端 `resolve_file_path`；这个函数只用于
 * 绝对路径输入，以及后端查不到（未索引文件）时按第一个根猜一个位置。
 */
export function resolveRelPath(relPath: string, roots: string[]): { rootPath: string; absPath: string } | null {
  const raw = relPath.trim().replace(/\\/g, '/')
  if (!raw) return null
  if (looksAbsolute(raw)) {
    const abs = cleanPath(raw)
    if (abs.split('/').includes('..')) return null
    const root = rootFor(abs, roots)
    return root ? { rootPath: root, absPath: abs } : null
  }
  const rel = raw.replace(/^\/+/, '')
  if (!rel || rel.split('/').includes('..')) return null
  const root = roots[0]
  if (!root) return null
  return { rootPath: cleanPath(root), absPath: joinPath(root, rel) }
}

/** `p` 是否在 `root` 之内（含 root 本身；Windows 大小写不敏感）。 */
export function isWithinRoot(p: string, root: string): boolean {
  const path = cleanPath(p).toLowerCase()
  const base = cleanPath(root).toLowerCase()
  if (path === base) return true
  return path.startsWith(base.endsWith('/') ? base : `${base}/`)
}

/** 路径是否落在任意一个资料库根内。 */
export function rootFor(path: string, roots: string[]): string | null {
  const hits = roots.filter(r => isWithinRoot(path, r))
  if (hits.length === 0) return null
  return hits.reduce((best, cur) => (cleanPath(cur).length > cleanPath(best).length ? cur : best))
}

export type DirSortKey = 'name' | 'mtime' | 'type' | 'size' | 'status' | 'quality'
export type SortOrder = 'asc' | 'desc'

/** 扩展名（文件夹返回空串）。 */
export function entryExt(entry: DirEntry): string {
  if (entry.is_dir) return ''
  const name = entry.name
  const idx = name.lastIndexOf('.')
  return idx > 0 ? name.slice(idx + 1).toLowerCase() : ''
}

/** 状态排序权重：失败最靠前（便于排查），其余按 已索引 → 处理中 → 未索引。 */
function statusWeight(entry: DirEntry): number {
  if (entry.is_dir) return 0
  if (!entry.is_supported) return 5
  if (entry.indexed === 2) return 1
  if (entry.indexed === 1) return 2
  if (entry.indexed === 3) return 3
  return 4
}

/**
 * 排序：**目录永远在文件前面**（资源管理器行为），组内按 key 比较。
 * `status`/`quality` 对目录一律视为相等，避免目录被质量分打散。
 */
export function sortEntries(entries: DirEntry[], key: DirSortKey, order: SortOrder): DirEntry[] {
  const dir = order === 'asc' ? 1 : -1
  const cmp = (a: DirEntry, b: DirEntry): number => {
    switch (key) {
      case 'mtime':
        return (a.mtime - b.mtime) * dir
      case 'size':
        return (a.file_size - b.file_size) * dir
      case 'type':
        return entryExt(a).localeCompare(entryExt(b)) * dir || a.name.localeCompare(b.name)
      case 'status':
        return (statusWeight(a) - statusWeight(b)) * dir || a.name.localeCompare(b.name)
      case 'quality': {
        const qa = a.is_dir ? -1 : (a.quality_score ?? -1)
        const qb = b.is_dir ? -1 : (b.quality_score ?? -1)
        return (qa - qb) * dir || a.name.localeCompare(b.name)
      }
      case 'name':
      default:
        return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' }) * dir
    }
  }
  return [...entries].sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1
    return cmp(a, b)
  })
}

/** 当前文件夹内按名字/路径过滤（客户端，不发请求）。 */
export function filterEntries(entries: DirEntry[], filter: string): DirEntry[] {
  const q = filter.trim().toLowerCase()
  if (!q) return entries
  return entries.filter(e => e.name.toLowerCase().includes(q) || e.path.toLowerCase().includes(q))
}

/** "文件夹" / 扩展名大写 / 不支持的类型。 */
export function entryTypeLabel(entry: DirEntry): string {
  if (entry.is_dir) return 'folder'
  if (!entry.is_supported) return 'unsupported'
  return entryExt(entry) || 'file'
}

/** 已索引状态徽标文案（组件里再翻成 i18n）。 */
export type EntryStatus = 'indexed' | 'extracting' | 'failed' | 'pending' | 'unsupported' | 'folder'

export function entryStatus(entry: DirEntry): EntryStatus {
  if (entry.is_dir) return 'folder'
  if (!entry.is_supported) return 'unsupported'
  if (entry.indexed === 1) return 'indexed'
  if (entry.indexed === 3) return 'extracting'
  if (entry.indexed === 2) return 'failed'
  return 'pending'
}

/** 合计大小（状态栏"已选择 N 个（共 X）"）。 */
export function totalSize(entries: DirEntry[]): number {
  return entries.reduce((sum, e) => sum + (e.is_dir ? 0 : e.file_size), 0)
}
