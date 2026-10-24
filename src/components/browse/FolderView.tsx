import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { listDirEntries, openFile, resolveFilePath, type DirEntry } from '../../api/files'
import type { DirConfig } from '../../api/dirs'
import { useI18n } from '../../i18n'
import { ChevronDownIcon, FolderIcon, RefreshIcon, SearchIcon } from '../../icons'
import { fileTypeIcon } from '../../utils/fileIcon'
import { formatSize, formatTime } from '../../utils/format'
import {
  breadcrumbs,
  canGoUp,
  cleanPath,
  entryStatus,
  entryTypeLabel,
  filterEntries,
  isSamePath,
  isWithinRoot,
  looksAbsolute,
  parentPath,
  resolveRelPath,
  rootFor,
  sortEntries,
  totalSize,
  type DirSortKey,
  type SortOrder,
} from './folderViewUtils'

const LS_LAST_DIR = 'ls_browse_dir'

interface FolderViewProps {
  /** 已注册的资料库（导航根）。 */
  roots: DirConfig[]
  /** 深链 `?path=`（库里存的相对资料库根的路径）。 */
  deepLinkPath: string | null
  /** 当前预览中的绝对路径（父组件维护，用来高亮行）。 */
  selectedPath: string | null
  /** 单击/键盘选中某一行（父组件据此更新预览面板）。 */
  onSelectEntry: (entry: DirEntry) => void
  /** 双击"用系统默认应用打开"失败/未索引时给提示。 */
  onNotice?: (message: string) => void
}

function loadLastDir(): string | null {
  try {
    return localStorage.getItem(LS_LAST_DIR)
  } catch {
    return null
  }
}

function saveLastDir(path: string) {
  try { localStorage.setItem(LS_LAST_DIR, path) } catch { /* 隐私模式忽略 */ }
}

/** 状态徽标：复用列表视图的视觉语言（✓ / ◐ / ✗ / ○）。 */
function StatusCell({ entry }: { entry: DirEntry }) {
  const { t } = useI18n()
  if (entry.is_dir) return <span className="text-gray-300 dark:text-gray-600">—</span>
  const status = entryStatus(entry)
  const base = 'inline-flex items-center gap-1 text-xs whitespace-nowrap'
  switch (status) {
    case 'indexed':
      return <span className={`${base} text-green-600 dark:text-green-400`}>✓ {t('indexed')}</span>
    case 'extracting':
      return <span className={`${base} text-yellow-600 dark:text-yellow-400`}>◐ {t('indexing')}</span>
    case 'failed':
      return <span className={`${base} text-red-600 dark:text-red-400`} title={entry.error_msg ?? undefined}>✗ {t('failed')}</span>
    case 'deleted':
      return <span className={`${base} text-gray-500 dark:text-gray-400`}>🗑 {t('deleted')}</span>
    case 'unsupported':
      return <span className={`${base} text-gray-400 dark:text-gray-500`}>{t('unsupported')}</span>
    default:
      return <span className={`${base} text-yellow-600 dark:text-yellow-400`}>○ {t('pending')}</span>
  }
}

function QualityCell({ entry }: { entry: DirEntry }) {
  if (entry.is_dir || entry.quality_score == null) {
    return <span className="text-gray-300 dark:text-gray-600">—</span>
  }
  const q = entry.quality_score
  const color = q > 0.75
    ? 'text-green-600 dark:text-green-400'
    : q >= 0.5
      ? 'text-yellow-600 dark:text-yellow-400'
      : 'text-red-600 dark:text-red-400'
  return <span className={`text-xs tabular-nums ${color}`}>{(q * 100).toFixed(0)}%</span>
}

/**
 * 文件夹视图（Windows 资源管理器风格）。
 *
 * 数据源是**真实文件系统**（`list_dir_entries`），因此能看见未索引文件、空目录与
 * 库里没追踪到的文件；索引状态/质量分由后端一次批量查询带出。
 * 全文搜索仍然归「搜索」页 —— 这里的过滤框只过滤当前文件夹已列出的条目。
 */
export default function FolderView({ roots, deepLinkPath, selectedPath, onSelectEntry, onNotice }: FolderViewProps) {
  const { t } = useI18n()

  // 导航状态：历史栈 + 当前位置（栈里存绝对路径）
  const [nav, setNav] = useState<{ stack: string[]; index: number }>({ stack: [], index: -1 })
  const currentPath = nav.stack[nav.index] ?? ''
  const initializedRef = useRef(false)

  const [entries, setEntries] = useState<DirEntry[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [reloadTick, setReloadTick] = useState(0)
  const [sort, setSort] = useState<{ key: DirSortKey; order: SortOrder }>({ key: 'name', order: 'asc' })
  const [filter, setFilter] = useState('')
  const [selection, setSelection] = useState<Set<string>>(new Set())
  const [focusedPath, setFocusedPath] = useState<string | null>(null)
  const anchorIndexRef = useRef<number | null>(null)
  const pendingSelectRef = useRef<string | null>(null)
  const tableRef = useRef<HTMLDivElement>(null)
  const filterRef = useRef<HTMLInputElement>(null)

  const rootPaths = useMemo(() => roots.map(r => r.path), [roots])

  /** 进入某个目录（新历史条目）。**绝不允许越出资料库根**（后端也会拒绝，双保险）。 */
  const navigate = useCallback((path: string, replace = false) => {
    const target = cleanPath(path)
    if (rootPaths.length > 0 && !rootFor(target, rootPaths)) {
      onNotice?.(t('path_outside_library'))
      return
    }
    setNav(prev => {
      const cur = prev.stack[prev.index]
      if (cur && isSamePath(cur, target)) return prev
      if (replace && prev.index >= 0) {
        const stack = [...prev.stack]
        stack[prev.index] = target
        return { stack, index: prev.index }
      }
      return { stack: [...prev.stack.slice(0, prev.index + 1), target], index: prev.index + 1 }
    })
    setSelection(new Set())
    setFocusedPath(null)
    setFilter('')
    anchorIndexRef.current = null
  }, [rootPaths, onNotice, t])

  // 首次进入：深链 > 上次目录 > 第一个资料库
  useEffect(() => {
    if (initializedRef.current || roots.length === 0) return
    initializedRef.current = true
    let cancelled = false

    const start = async () => {
      if (deepLinkPath) {
        // 绝对路径直接可用；相对路径要靠 DB 找归属（多资料库时只有它知道是哪个根）
        let abs: string | null = looksAbsolute(deepLinkPath) ? cleanPath(deepLinkPath) : null
        if (!abs) {
          abs = await resolveFilePath(deepLinkPath).catch(() => null)
        }
        if (cancelled) return
        if (!abs || !rootFor(abs, rootPaths)) {
          // 后端查不到（未索引文件）→ 按第一个根猜路径，至少能定位到父目录
          const guess = resolveRelPath(deepLinkPath, rootPaths)
          abs = guess?.absPath ?? null
        }
        if (cancelled) return
        if (abs) {
          const target = cleanPath(abs)
          setNav({ stack: [parentPath(target)], index: 0 })
          setSelection(new Set([target]))
          pendingSelectRef.current = target
          return
        }
      }
      const saved = loadLastDir()
      const startPath = saved && roots.some(r => isWithinRoot(saved, r.path))
        ? cleanPath(saved)
        : cleanPath(roots[0]!.path)
      setNav({ stack: [startPath], index: 0 })
    }

    void start()
    return () => { cancelled = true }
  }, [roots, rootPaths, deepLinkPath])

  // 加载当前目录
  useEffect(() => {
    if (!currentPath) return
    let cancelled = false
    setLoading(true)
    setError(null)
    listDirEntries(currentPath)
      .then(list => {
        if (cancelled) return
        setEntries(list)
        saveLastDir(currentPath)
      })
      .catch((e: unknown) => {
        if (cancelled) return
        setEntries([])
        setError(e instanceof Error ? e.message : t('folder_load_failed'))
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => { cancelled = true }
  }, [currentPath, reloadTick, t])

  const visibleEntries = useMemo(
    () => sortEntries(filterEntries(entries, filter), sort.key, sort.order),
    [entries, filter, sort.key, sort.order],
  )

  const rootPath = useMemo(() => rootFor(currentPath, rootPaths), [currentPath, rootPaths])
  const rootLabel = useMemo(() => {
    if (!rootPath) return t('select_library')
    const cfg = roots.find(r => isSamePath(r.path, rootPath))
    return cfg?.alias?.trim() || rootPath.split('/').filter(Boolean).pop() || rootPath
  }, [rootPath, roots, t])
  const crumbs = useMemo(() => breadcrumbs(rootPath ?? currentPath, rootLabel, currentPath), [rootPath, rootLabel, currentPath])

  // 深链落到具体文件后：等目录加载完再选中并预览
  useEffect(() => {
    const pending = pendingSelectRef.current
    if (!pending || loading || entries.length === 0) return
    const hit = entries.find(e => isSamePath(e.path, pending))
    if (hit) {
      pendingSelectRef.current = null
      setFocusedPath(hit.path)
      if (!hit.is_dir) onSelectEntry(hit)
      requestAnimationFrame(() => scrollToPath(hit.path))
    }
  }, [entries, loading, onSelectEntry])

  const scrollToPath = (path: string) => {
    const el = tableRef.current?.querySelector(`[data-entry-path="${CSS.escape(path)}"]`)
    el?.scrollIntoView({ block: 'nearest' })
  }

  const selectEntry = useCallback((entry: DirEntry, additive: boolean) => {
    setFocusedPath(entry.path)
    setSelection(prev => {
      if (!additive) return new Set([entry.path])
      const next = new Set(prev)
      if (next.has(entry.path)) next.delete(entry.path)
      else next.add(entry.path)
      return next
    })
    // 目录只在列表里选中（预览面板是文件预览，不展示文件夹属性）
    if (!entry.is_dir) onSelectEntry(entry)
  }, [onSelectEntry])

  const openEntry = useCallback((entry: DirEntry) => {
    if (entry.is_dir) {
      navigate(entry.path)
      return
    }
    if (entry.file_id) {
      void openFile(entry.file_id).catch(e => {
        onNotice?.(e instanceof Error ? e.message : String(e))
      })
    } else {
      onNotice?.(t('file_not_indexed_yet'))
    }
  }, [navigate, onNotice, t])

  const refresh = useCallback(() => setReloadTick(v => v + 1), [])
  // 进入/切换目录后把键盘焦点收进列表：↑↓/Enter/Backspace 立刻可用
  useEffect(() => {
    if (currentPath) tableRef.current?.focus({ preventScroll: true })
  }, [currentPath])
  const goUp = useCallback(() => {
    // 到资料库根就停住：不加这个判断会走到 `D:/`，等于把整块磁盘暴露出来
    if (!canGoUp(currentPath, rootPath ?? '')) return
    navigate(parentPath(currentPath))
  }, [currentPath, rootPath, navigate])
  const goBack = useCallback(() => setNav(p => (p.index > 0 ? { ...p, index: p.index - 1 } : p)), [])
  const goForward = useCallback(
    () => setNav(p => (p.index < p.stack.length - 1 ? { ...p, index: p.index + 1 } : p)),
    [],
  )

  /** 键盘移动（↑↓）与资源管理器一致：只走当前可见列表。 */
  const moveFocus = useCallback((delta: 1 | -1) => {
    if (visibleEntries.length === 0) return
    const cur = focusedPath ? visibleEntries.findIndex(e => isSamePath(e.path, focusedPath)) : -1
    const next = cur < 0
      ? (delta > 0 ? 0 : visibleEntries.length - 1)
      : Math.min(visibleEntries.length - 1, Math.max(0, cur + delta))
    const entry = visibleEntries[next]!
    anchorIndexRef.current = next
    setFocusedPath(entry.path)
    setSelection(new Set([entry.path]))
    if (!entry.is_dir) onSelectEntry(entry)
    requestAnimationFrame(() => scrollToPath(entry.path))
  }, [visibleEntries, focusedPath, onSelectEntry])

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.target instanceof HTMLInputElement) return
    if (e.altKey && e.key === 'ArrowLeft') { e.preventDefault(); goBack(); return }
    if (e.altKey && e.key === 'ArrowRight') { e.preventDefault(); goForward(); return }
    switch (e.key) {
      case 'ArrowDown': e.preventDefault(); moveFocus(1); break
      case 'ArrowUp': e.preventDefault(); moveFocus(-1); break
      case 'Enter': {
        const entry = visibleEntries.find(x => focusedPath != null && isSamePath(x.path, focusedPath))
        if (entry) { e.preventDefault(); openEntry(entry) }
        break
      }
      case 'Backspace': e.preventDefault(); goUp(); break
      case 'F5': e.preventDefault(); refresh(); break
      case 'a':
      case 'A':
        if (e.ctrlKey || e.metaKey) {
          e.preventDefault()
          setSelection(new Set(visibleEntries.map(x => x.path)))
        }
        break
      case 'Escape':
        setSelection(new Set())
        setFocusedPath(null)
        break
      default:
        break
    }
  }

  const toggleSort = (key: DirSortKey) => {
    setSort(prev => (prev.key === key ? { key, order: prev.order === 'asc' ? 'desc' : 'asc' } : { key, order: 'asc' }))
  }

  const canBack = nav.index > 0
  const canForward = nav.index >= 0 && nav.index < nav.stack.length - 1
  const canUp = canGoUp(currentPath, rootPath ?? '')
  const selectedEntries = useMemo(
    () => visibleEntries.filter(e => selection.has(e.path)),
    [visibleEntries, selection],
  )
  const selectedSize = useMemo(() => totalSize(selectedEntries), [selectedEntries])

  const navBtn = 'p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-800 text-gray-500 dark:text-gray-400 disabled:opacity-30 disabled:cursor-not-allowed transition-colors'

  const sortIndicator = (key: DirSortKey) => (sort.key === key ? (sort.order === 'asc' ? ' ▲' : ' ▼') : '')

  if (roots.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-gray-400 dark:text-gray-500">
        {t('no_dirs')}
      </div>
    )
  }

  return (
    <div className="flex flex-col flex-1 min-h-0">
      {/* 导航栏：前进/后退/上一级 + 面包屑 + 刷新 + 当前文件夹过滤 */}
      <div className="flex items-center gap-1.5 px-3 py-2 border-b border-gray-200 dark:border-gray-800 flex-wrap">
        <button onClick={goBack} disabled={!canBack} className={navBtn} title={t('nav_back')} aria-label={t('nav_back')}>
          <svg className="size-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m15 18-6-6 6-6" /></svg>
        </button>
        <button onClick={goForward} disabled={!canForward} className={navBtn} title={t('nav_forward')} aria-label={t('nav_forward')}>
          <svg className="size-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m9 18 6-6-6-6" /></svg>
        </button>
        <button onClick={goUp} disabled={!canUp} className={navBtn} title={t('up_one_level')} aria-label={t('up_one_level')}>
          <svg className="size-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m18 15-6-6-6 6" /><path d="M12 9v10" /></svg>
        </button>

        {/* 面包屑（窄面板自动横向滚动） */}
        <div className="flex items-center gap-0.5 min-w-0 flex-1 overflow-x-auto whitespace-nowrap text-xs">
          {crumbs.map((c, i) => (
            <span key={c.path} className="inline-flex items-center shrink-0">
              {i > 0 && <ChevronDownIcon className="size-3 -rotate-90 text-gray-300 dark:text-gray-600" />}
              <button
                onClick={() => navigate(c.path)}
                className={`px-1 py-0.5 rounded hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors ${
                  i === crumbs.length - 1 ? 'text-gray-700 dark:text-gray-200' : 'text-gray-500 dark:text-gray-400'
                }`}
                title={c.path}
              >
                {c.label}
              </button>
            </span>
          ))}
        </div>

        {roots.length > 1 && (
          <select
            value={rootPath ?? ''}
            onChange={e => navigate(e.target.value)}
            className="text-xs bg-transparent border border-gray-200 dark:border-gray-700 rounded px-1.5 py-1 text-gray-600 dark:text-gray-400 max-w-[140px]"
            title={t('select_library')}
          >
            {roots.map(r => (
              <option key={r.id} value={r.path}>{r.alias?.trim() || r.path}</option>
            ))}
          </select>
        )}

        <div className="relative">
          <SearchIcon className="absolute left-1.5 top-1/2 -translate-y-1/2 size-3 text-gray-400" />
          <input
            ref={filterRef}
            value={filter}
            onChange={e => setFilter(e.target.value)}
            placeholder={t('filter_current_folder')}
            className="w-40 text-xs bg-transparent border border-gray-200 dark:border-gray-700 rounded pl-6 pr-6 py-1 text-gray-700 dark:text-gray-300 placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500"
          />
          {filter && (
            <button
              onClick={() => { setFilter(''); filterRef.current?.focus() }}
              className="absolute right-1 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-200"
              aria-label={t('clear_search')}
            >
              ×
            </button>
          )}
        </div>

        <button onClick={refresh} className={navBtn} title={t('refresh')} aria-label={t('refresh')}>
          <RefreshIcon className="size-4" />
        </button>
      </div>

      {/* 文件列表（资源管理器"详细信息"视图） */}
      <div
        ref={tableRef}
        tabIndex={0}
        onKeyDown={handleKeyDown}
        onMouseDown={e => {
          // 点行时不抢输入框焦点，只把键盘焦点收进列表（Explorer 的 ↑↓ 才生效）
          ;(e.currentTarget as HTMLDivElement).focus()
        }}
        onClick={e => {
          if (e.target === e.currentTarget) {
            setSelection(new Set())
            setFocusedPath(null)
          }
        }}
        className="flex-1 min-h-0 overflow-auto outline-none"
      >
        {loading ? (
          <div className="flex items-center justify-center py-16">
            <span className="size-5 border-2 border-gray-300 border-t-blue-500 rounded-full animate-spin" />
          </div>
        ) : error ? (
          <div className="p-6 space-y-3">
            <p className="text-sm text-red-600 dark:text-red-400">{error}</p>
            {canUp && (
              <button onClick={goUp} className="px-3 py-1.5 text-xs font-medium text-gray-600 dark:text-gray-300 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700">
                ← {t('up_one_level')}
              </button>
            )}
          </div>
        ) : visibleEntries.length === 0 ? (
          <div className="flex items-center justify-center py-16 text-sm text-gray-400 dark:text-gray-500">
            {filter ? t('no_results_found') : t('folder_empty')}
          </div>
        ) : (
          <table className="w-full text-xs select-none table-fixed">
            <thead className="sticky top-0 bg-gray-50 dark:bg-gray-900/90 backdrop-blur z-10">
              <tr className="border-b border-gray-200 dark:border-gray-800 text-left text-gray-500 dark:text-gray-400">
                <th className="px-2 py-1.5 font-medium cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-800" style={{ width: '32%' }} onClick={() => toggleSort('name')}>
                  {t('filename')}{sortIndicator('name')}
                </th>
                <th className="hidden lg:table-cell px-2 py-1.5 font-medium cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-800" style={{ width: '14%' }} onClick={() => toggleSort('mtime')}>
                  {t('modified')}{sortIndicator('mtime')}
                </th>
                <th className="hidden md:table-cell px-2 py-1.5 font-medium cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-800" style={{ width: '14%' }} onClick={() => toggleSort('indexed_at')}>
                  {t('last_indexed')}{sortIndicator('indexed_at')}
                </th>
                <th className="hidden xl:table-cell px-2 py-1.5 font-medium cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-800" style={{ width: '8%' }} onClick={() => toggleSort('type')}>
                  {t('type')}{sortIndicator('type')}
                </th>
                <th className="px-2 py-1.5 font-medium text-right cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-800" style={{ width: '8%' }} onClick={() => toggleSort('size')}>
                  {t('size')}{sortIndicator('size')}
                </th>
                <th className="px-2 py-1.5 font-medium cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-800" style={{ width: '16%' }} onClick={() => toggleSort('status')}>
                  {t('status')}{sortIndicator('status')}
                </th>
                <th className="hidden md:table-cell px-2 py-1.5 font-medium cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-800" style={{ width: '8%' }} onClick={() => toggleSort('quality')}>
                  {t('quality')}{sortIndicator('quality')}
                </th>
              </tr>
            </thead>
            <tbody>
              {visibleEntries.map(entry => {
                const isSelected = selection.has(entry.path)
                const isFocused = focusedPath != null && isSamePath(focusedPath, entry.path)
                const isPreviewed = selectedPath != null && isSamePath(selectedPath, entry.path)
                return (
                  <tr
                    key={entry.path}
                    data-entry-path={entry.path}
                    onClick={e => selectEntry(entry, e.ctrlKey || e.metaKey)}
                    onDoubleClick={() => openEntry(entry)}
                    className={`border-b border-gray-100 dark:border-gray-800/50 cursor-pointer transition-colors ${
                      isSelected
                        ? 'bg-blue-50 dark:bg-blue-900/20'
                        : isPreviewed
                          ? 'bg-blue-50/60 dark:bg-blue-900/10'
                          : 'hover:bg-gray-50 dark:hover:bg-gray-800/50'
                    } ${isFocused ? 'ring-1 ring-inset ring-blue-400/60' : ''}`}
                  >
                    <td className="px-2 py-1">
                      <div className="flex items-center gap-1.5 min-w-0">
                        {entry.is_dir
                          ? <FolderIcon className="size-3.5 text-amber-500 shrink-0" />
                          : fileTypeIcon(entry.path, { className: `size-3.5 shrink-0 ${entry.is_supported ? 'text-gray-400' : 'text-gray-300 dark:text-gray-600'}` })}
                        <span
                          className={`truncate ${entry.is_dir ? 'text-gray-800 dark:text-gray-200' : entry.is_supported ? '' : 'text-gray-400 dark:text-gray-500'}`}
                          title={entry.name}
                        >
                          {entry.name}
                        </span>
                      </div>
                    </td>
                    <td className="hidden lg:table-cell px-2 py-1 text-gray-500 dark:text-gray-400 whitespace-nowrap">
                      {entry.mtime > 0 ? formatTime(entry.mtime) : '—'}
                    </td>
                    <td className="hidden md:table-cell px-2 py-1 text-gray-500 dark:text-gray-400 whitespace-nowrap">
                      {entry.indexed_at != null ? formatTime(entry.indexed_at) : '—'}
                    </td>
                    <td className="hidden xl:table-cell px-2 py-1 text-gray-500 dark:text-gray-400 uppercase truncate">
                      {(() => {
                        const label = entryTypeLabel(entry)
                        if (label === 'folder') return t('folder_type_label')
                        if (label === 'unsupported') return t('unsupported')
                        if (label === 'file') return t('file_type_label')
                        return label.toUpperCase()
                      })()}
                    </td>
                    <td className="px-2 py-1 text-right text-gray-500 dark:text-gray-400 tabular-nums whitespace-nowrap">
                      {entry.is_dir ? '—' : formatSize(entry.file_size)}
                    </td>
                    <td className="px-2 py-1"><StatusCell entry={entry} /></td>
                    <td className="hidden md:table-cell px-2 py-1"><QualityCell entry={entry} /></td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        )}
      </div>

      {/* 状态栏 */}
      <div className="flex items-center justify-between px-3 py-1.5 border-t border-gray-200 dark:border-gray-800 text-xs text-gray-500 dark:text-gray-400 shrink-0">
        <span>{t('items_count', { count: visibleEntries.length })}</span>
        <span className="tabular-nums">
          {selection.size > 0
            ? `${t('selected_items', { count: selection.size })}${selectedSize > 0 ? ` · ${formatSize(selectedSize)}` : ''}`
            : ''}
        </span>
      </div>
    </div>
  )
}
