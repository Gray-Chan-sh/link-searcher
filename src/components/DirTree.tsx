import { useEffect, useRef, useState } from 'react'
import { ChevronDownIcon, FolderIcon } from '../icons'
import type { DirTreeNode } from '../api/dirs'

interface DirTreeProps {
  node: DirTreeNode
  selectedPaths: Set<string>
  onToggle: (path: string, checked: boolean) => void
}

function hasSelectedDescendant(node: DirTreeNode, selectedPaths: Set<string>): boolean {
  return node.children.some(
    child => selectedPaths.has(child.path) || hasSelectedDescendant(child, selectedPaths),
  )
}

export default function DirTree({ node, selectedPaths, onToggle }: DirTreeProps) {
  const [expanded, setExpanded] = useState(false)
  const checkboxRef = useRef<HTMLInputElement>(null)

  const isChecked = selectedPaths.has(node.path)
  const isIndeterminate = !isChecked && hasSelectedDescendant(node, selectedPaths)
  const hasChildren = node.children.length > 0

  useEffect(() => {
    if (checkboxRef.current) {
      checkboxRef.current.indeterminate = isIndeterminate
    }
  }, [isIndeterminate])

  return (
    <div>
      <div className="flex items-center gap-1 py-0.5">
        <button
          onClick={() => setExpanded(v => !v)}
          className="size-4 flex items-center justify-center text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors shrink-0"
          aria-label={expanded ? 'Collapse' : 'Expand'}
        >
          {hasChildren && (
            <ChevronDownIcon
              className={`size-3 transition-transform duration-150 ${expanded ? '' : '-rotate-90'}`}
            />
          )}
        </button>
        <input
          ref={checkboxRef}
          type="checkbox"
          checked={isChecked}
          onChange={e => onToggle(node.path, e.target.checked)}
          className="rounded border-gray-300 dark:border-gray-600 text-blue-600 focus:ring-blue-500 dark:bg-gray-700 shrink-0"
        />
        <FolderIcon className="size-3.5 text-gray-400 dark:text-gray-500 shrink-0" />
        <span className="text-xs text-gray-700 dark:text-gray-300 truncate group-hover:text-gray-900 dark:group-hover:text-gray-100 transition-colors">
          {node.name}
        </span>
      </div>
      {hasChildren && expanded && (
        <div className="ml-5 overflow-hidden">
          {node.children.map(child => (
            <DirTree
              key={child.path}
              node={child}
              selectedPaths={selectedPaths}
              onToggle={onToggle}
            />
          ))}
        </div>
      )}
    </div>
  )
}
