import { useDirs } from '../hooks/useDirs'
import { addDir } from '../api/dirs'
import { PlusIcon, TrashIcon, FolderIcon } from '../icons'
import EmptyState from '../components/EmptyState'
import { ListSkeleton } from '../components/Skeleton'
import { ask } from '@tauri-apps/plugin-dialog'

export default function DirManager() {
  const { dirs, loading, error, addDirectory, removeDirectory, refresh } = useDirs()

  const handleDrop = async (e: React.DragEvent) => {
    e.preventDefault()
    const path = e.dataTransfer.getData('text/plain')
    if (path) {
      await addDir(path, undefined, true)
      await refresh()
    }
  }

  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault()
  }

  return (
    <div className="h-full p-6" onDrop={handleDrop} onDragOver={handleDragOver}>
      <div className="flex items-center justify-between mb-6">
        <div>
          <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">Directories</h2>
          <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
            Manage the directories indexed for search
          </p>
        </div>
        <button
          onClick={addDirectory}
          className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors"
        >
          <PlusIcon className="size-4" />
          Add Directory
        </button>
      </div>

      {loading && (
        <ListSkeleton rows={4} />
      )}

      {error && (
        <div className="px-4 py-3 mb-4 text-sm text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-900 rounded-lg">
          {error}
        </div>
      )}

      {!loading && dirs.length === 0 && (
        <EmptyState
          icon={<FolderIcon className="size-12" />}
          title="No directories added"
          description="Add a directory to start indexing your documents"
          action={
            <button
              onClick={addDirectory}
              className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded-lg hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
            >
              <PlusIcon className="size-4" />
              Add your first directory
            </button>
          }
        />
      )}

      {!loading && dirs.length > 0 && (
        <div className="space-y-3">
          {dirs.map(dir => (
            <div
              key={dir.id}
              className="flex items-start gap-4 p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg"
            >
              <FolderIcon className="size-5 text-gray-400 mt-0.5 shrink-0" />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100">
                    {dir.alias ?? dir.path.split('/').pop() ?? dir.path}
                  </h3>
                  {dir.recursive && (
                    <span className="text-xs text-gray-400 dark:text-gray-500">(recursive)</span>
                  )}
                </div>
                <p className="text-xs text-gray-500 dark:text-gray-400 truncate mt-0.5 font-mono">{dir.path}</p>
                <div className="flex items-center gap-3 mt-2 text-xs text-gray-400 dark:text-gray-500">
                  <span>OCR: {dir.ocr_lang}</span>
                  {dir.include_exts && <span>Exts: {dir.include_exts}</span>}
                </div>
              </div>
              <button
                onClick={async () => {
                  const confirmed = await ask('确认删除此目录？', { title: '删除资料库', kind: 'warning' })
                  if (confirmed) {
                    removeDirectory(dir.id)
                  }
                }}
                className="p-1.5 rounded-md text-gray-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/20 transition-colors shrink-0"
                title="Remove directory"
              >
                <TrashIcon className="size-4" />
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
