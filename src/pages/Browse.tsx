import { useEffect, useState, useCallback } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { convertFileSrc } from '@tauri-apps/api/core'
import { getFilePreview, type FilePreview } from '../api/files'
import { FolderIcon, FileTextIcon, ChevronDownIcon, XIcon } from '../icons'

interface DirEntry {
  name: string
  path: string
  is_dir: boolean
  is_supported: boolean
  file_size: number
  mtime: number
  indexed: boolean
}

interface TreeNode {
  name: string
  path: string
  children: TreeNode[]
  expanded: boolean
  loading: boolean
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

function formatTime(ts: number): string {
  return new Date(ts / 1000).toLocaleString()
}

export default function Browse() {
  const [trees, setTrees] = useState<TreeNode[]>([])
  const [selectedDir, setSelectedDir] = useState<string | null>(null)
  const [entries, setEntries] = useState<DirEntry[]>([])
  const [entriesLoading, setEntriesLoading] = useState(false)
  const [selectedFile, setSelectedFile] = useState<string | null>(null)
  const [preview, setPreview] = useState<FilePreview | null>(null)
  const [previewLoading, setPreviewLoading] = useState(false)
  const [previewError, setPreviewError] = useState<string | null>(null)

  // Load root directories on mount
  useEffect(() => {
    invoke<{ id: string; path: string }[]>('list_dirs')
      .then(dirs => {
        setTrees(dirs.map(d => ({
          name: d.path.split('/').filter(Boolean).pop() || d.path,
          path: d.path,
          children: [],
          expanded: false,
          loading: false,
        })))
      })
      .catch(() => {})
  }, [])

  const toggleDir = useCallback(async (node: TreeNode) => {
    if (node.children.length > 0) {
      setTrees(prev => toggleNode(prev, node.path, { expanded: !node.expanded }))
      return
    }

    setTrees(prev => toggleNode(prev, node.path, { loading: true, expanded: true }))

    try {
      const entries: DirEntry[] = await invoke('list_dir_entries', { path: node.path })
      const dirs = entries.filter(e => e.is_dir).map(e => ({
        name: e.name,
        path: e.path,
        children: [] as TreeNode[],
        expanded: false,
        loading: false,
      }))
      setTrees(prev => toggleNode(prev, node.path, { children: dirs, loading: false }))
    } catch {
      setTrees(prev => toggleNode(prev, node.path, { loading: false }))
    }
  }, [])

  const selectDir = useCallback(async (path: string) => {
    setSelectedDir(path)
    setSelectedFile(null)
    setPreview(null)
    setPreviewError(null)
    setEntriesLoading(true)

    try {
      const entries: DirEntry[] = await invoke('list_dir_entries', { path })
      setEntries(entries)
    } catch (e) {
      setEntries([])
    }
    setEntriesLoading(false)
  }, [])

  const selectFile = useCallback(async (path: string) => {
    setSelectedFile(path)
    setPreviewLoading(true)
    setPreviewError(null)
    setPreview(null)

    try {
      const result: FilePreview = await invoke('preview_file_by_path', { path })
      setPreview(result)
    } catch (e) {
      setPreviewError(typeof e === 'string' ? e : 'Failed to load preview')
    }
    setPreviewLoading(false)
  }, [])

  return (
    <div className="flex h-full">
      {/* Left: Directory tree */}
      <div className="w-60 shrink-0 border-r border-gray-200 dark:border-gray-800 bg-gray-50 dark:bg-gray-900/50 overflow-y-auto">
        <div className="px-3 py-3 border-b border-gray-200 dark:border-gray-800">
          <h3 className="text-xs font-semibold text-gray-500 dark:text-gray-400 uppercase tracking-wider">Directories</h3>
        </div>
        <div className="py-1">
          {trees.map(node => (
            <DirTreeNode
              key={node.path}
              node={node}
              onToggle={toggleDir}
              onSelect={selectDir}
              selectedPath={selectedDir}
            />
          ))}
          {trees.length === 0 && (
            <p className="px-3 py-4 text-xs text-gray-400 text-center">No directories added</p>
          )}
        </div>
      </div>

      {/* Middle: File list */}
      <div className="w-64 shrink-0 border-r border-gray-200 dark:border-gray-800 overflow-y-auto">
        <div className="px-3 py-3 border-b border-gray-200 dark:border-gray-800">
          <h3 className="text-xs font-semibold text-gray-500 dark:text-gray-400 uppercase tracking-wider">
            {selectedDir ? selectedDir.split('/').pop() : 'Files'}
          </h3>
        </div>
        {entriesLoading ? (
          <div className="p-4 text-xs text-gray-400">Loading...</div>
        ) : (
          <div className="py-1">
            {entries.map(e => (
              <button
                key={e.path}
                onClick={() => e.is_dir ? selectDir(e.path) : selectFile(e.path)}
                className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs text-left transition-colors ${
                  selectedFile === e.path
                    ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-700 dark:text-blue-300'
                    : 'text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800'
                } ${e.is_dir ? 'font-medium' : ''}`}
              >
                {e.is_dir ? (
                  <FolderIcon className="size-3.5 shrink-0 text-amber-500" />
                ) : (
                  <FileTextIcon className={`size-3.5 shrink-0 ${e.indexed ? 'text-green-500' : 'text-gray-400'}`} />
                )}
                <span className="truncate flex-1">{e.name}</span>
                {!e.is_dir && (
                  <span className="text-gray-400 shrink-0">{formatSize(e.file_size)}</span>
                )}
              </button>
            ))}
            {!entriesLoading && entries.length === 0 && selectedDir && (
              <p className="px-3 py-4 text-xs text-gray-400 text-center">Empty directory</p>
            )}
          </div>
        )}
      </div>

      {/* Right: Preview */}
      <div className="flex-1 overflow-y-auto bg-white dark:bg-gray-900">
        {previewLoading && (
          <div className="flex items-center justify-center py-16">
            <div className="size-5 border-2 border-gray-300 dark:border-gray-600 border-t-blue-500 rounded-full animate-spin" />
          </div>
        )}

        {previewError && (
          <div className="p-6 text-sm text-red-600 dark:text-red-400">
            {previewError}
          </div>
        )}

        {preview && !previewLoading && (
          <div className="p-6">
            {preview.file_type === 'image' && preview.image_path && (
              <div className="mb-4 flex justify-center">
                <img
                  src={convertFileSrc(preview.image_path)}
                  alt=""
                  className="max-w-full max-h-96 object-contain rounded-lg border border-gray-200 dark:border-gray-700"
                />
              </div>
            )}

            {preview.content && (
              <pre className="text-sm text-gray-700 dark:text-gray-300 whitespace-pre-wrap font-mono leading-relaxed">
                {preview.content}
              </pre>
            )}

            {!preview.content && preview.file_type !== 'image' && (
              <p className="text-sm text-gray-400">No preview available</p>
            )}

            {preview.char_count > 0 && (
              <p className="mt-4 text-xs text-gray-400 border-t border-gray-200 dark:border-gray-800 pt-2">
                {preview.char_count} characters {preview.ocr_used ? '(OCR)' : ''}
              </p>
            )}
          </div>
        )}

        {!preview && !previewLoading && !previewError && selectedFile && (
          <p className="p-6 text-sm text-gray-400">Loading preview...</p>
        )}

        {!selectedFile && !previewLoading && (
          <div className="flex items-center justify-center h-full text-sm text-gray-400">
            Select a file to preview
          </div>
        )}
      </div>
    </div>
  )
}

function toggleNode(nodes: TreeNode[], path: string, patch: Partial<TreeNode>): TreeNode[] {
  return nodes.map(n => {
    if (n.path === path) return { ...n, ...patch }
    if (n.children.length > 0) return { ...n, children: toggleNode(n.children, path, patch) }
    return n
  })
}

function DirTreeNode({ node, onToggle, onSelect, selectedPath }: {
  node: TreeNode
  onToggle: (n: TreeNode) => void
  onSelect: (path: string) => void
  selectedPath: string | null
}) {
  return (
    <div>
      <div
        className={`flex items-center gap-1 px-3 py-1 text-xs cursor-pointer transition-colors ${
          selectedPath === node.path
            ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-700 dark:text-blue-300'
            : 'text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800'
        }`}
      >
        <button
          onClick={(e) => { e.stopPropagation(); onToggle(node) }}
          className={`p-0.5 rounded transition-colors hover:bg-gray-200 dark:hover:bg-gray-700 ${
            node.loading ? 'animate-spin' : ''
          }`}
        >
          <ChevronDownIcon
            className={`size-3 text-gray-400 transition-transform ${node.expanded ? '' : '-rotate-90'}`}
          />
        </button>
        <FolderIcon className="size-3.5 shrink-0 text-amber-500" />
        <button
          onClick={() => onSelect(node.path)}
          className="truncate flex-1 text-left"
          title={node.path}
        >
          {node.name}
        </button>
      </div>
      {node.expanded && node.children.length > 0 && (
        <div className="ml-3">
          {node.children.map(child => (
            <DirTreeNode
              key={child.path}
              node={child}
              onToggle={onToggle}
              onSelect={onSelect}
              selectedPath={selectedPath}
            />
          ))}
        </div>
      )}
    </div>
  )
}
