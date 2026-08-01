import { useEffect, useMemo, useState } from 'react'
import type { DirConfig, DirTreeNode } from '../api/dirs'
import { getDirTree } from '../api/dirs'
import { getFileTypeStats, type FileTypeStat } from '../api/search'
import { useI18n } from '../i18n'
import DirTree from './DirTree'

interface FilterPanelProps {
  dirs: DirConfig[]
  dirPaths: string[]
  extFilter: string[]
  onDirPathsChange: (paths: string[]) => void
  onExtToggle: (ext: string) => void
  onClearFilters: () => void
}

const COMMON_EXTS = ['pdf', 'docx', 'txt', 'md', 'html', 'csv', 'json', 'xml', 'jpg', 'png']

function collectAllPaths(node: DirTreeNode): string[] {
  return [node.path, ...node.children.flatMap(collectAllPaths)]
}

function findNode(trees: DirTreeNode[], path: string): DirTreeNode | null {
  for (const tree of trees) {
    if (tree.path === path) return tree
    const found = findNode(tree.children, path)
    if (found) return found
  }
  return null
}

export default function FilterPanel({
  dirs,
  dirPaths,
  extFilter,
  onDirPathsChange,
  onExtToggle,
  onClearFilters,
}: FilterPanelProps) {
  const { t } = useI18n()
  const [trees, setTrees] = useState<DirTreeNode[]>([])
  const [typeStats, setTypeStats] = useState<FileTypeStat[]>([])

  useEffect(() => {
    getFileTypeStats()
      .then(setTypeStats)
      .catch(e => console.error('Failed to load file type stats:', e))
  }, [])

  useEffect(() => {
    if (dirs.length === 0) {
      setTrees([])
      return
    }
    let cancelled = false
    Promise.all(dirs.map(d => getDirTree(d.id)))
      .then(results => {
        if (!cancelled) setTrees(results)
      })
      .catch(err => {
        console.error('Failed to load dir trees:', err)
        if (!cancelled) setTrees([])
      })
    return () => { cancelled = true }
  }, [dirs])

  const selectedSet = useMemo(() => new Set(dirPaths), [dirPaths])
  const hasFilters = dirPaths.length > 0 || extFilter.length > 0

  const handleToggle = (path: string, checked: boolean) => {
    const node = findNode(trees, path)
    if (!node) return
    const allPaths = collectAllPaths(node)
    const next = new Set(dirPaths)
    if (checked) {
      allPaths.forEach(p => next.add(p))
    } else {
      allPaths.forEach(p => next.delete(p))
    }
    onDirPathsChange(Array.from(next))
  }

  return (
    <div role="region" aria-label={t('filters')} className="w-56 shrink-0 border-r border-gray-200 dark:border-gray-800 bg-gray-50/50 dark:bg-gray-900/50 p-4 overflow-y-auto">
      <div className="flex items-center justify-between mb-3">
        <h3 className="text-xs font-semibold text-gray-500 dark:text-gray-400 uppercase tracking-wider">{t('filters')}</h3>
        {hasFilters && (
          <button onClick={onClearFilters} className="text-xs text-blue-600 dark:text-blue-400 hover:underline">
            {t('clear')}
          </button>
        )}
      </div>

      {trees.length > 0 && (
        <div className="mb-4">
          <h4 className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-2">{t('directories')}</h4>
          <div className="space-y-0.5">
            {trees.map(tree => (
              <DirTree
                key={tree.path}
                node={tree}
                selectedPaths={selectedSet}
                onToggle={handleToggle}
              />
            ))}
          </div>
        </div>
      )}

      <div>
        <h4 className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-2">{t('file_type')}</h4>
        <div className="space-y-1">
          {[...new Set(
            typeStats.length > 0
              ? typeStats.map(s => s.extension)
              : COMMON_EXTS
          )].map(ext => (
            <label key={ext} className="flex items-center gap-2 cursor-pointer group">
              <input
                type="checkbox"
                checked={extFilter.includes(ext)}
                onChange={() => onExtToggle(ext)}
                className="rounded border-gray-300 dark:border-gray-600 text-blue-600 focus:ring-blue-500 dark:bg-gray-700"
              />
              <span className="text-xs text-gray-700 dark:text-gray-300 group-hover:text-gray-900 dark:group-hover:text-gray-100 transition-colors">
                .{ext}
              </span>
            </label>
          ))}
        </div>
      </div>
    </div>
  )
}
