import { useDirs } from '../hooks/useDirs'
import { addDir, updateDir } from '../api/dirs'
import { PlusIcon, TrashIcon, FolderIcon } from '../icons'
import { useI18n } from '../i18n'
import EmptyState from '../components/EmptyState'
import { ListSkeleton } from '../components/Skeleton'
import { ask } from '@tauri-apps/plugin-dialog'

export default function DirManager() {
  const { t } = useI18n()
  const { dirs, loading, error, addDirectory, removeDirectory, refresh } = useDirs()

  const handleTogglePrivate = async (dirId: string, current: boolean) => {
    try {
      await updateDir(dirId, undefined, undefined, undefined, undefined, undefined, !current)
      await refresh()
    } catch { /* ignore */ }
  }

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
          <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">{t('directories')}</h2>
          <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
            {t('manage_directories')}
          </p>
        </div>
        <button
          onClick={addDirectory}
          className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors"
        >
          <PlusIcon className="size-4" />
          {t('add_directory')}
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
          title={t('no_directories')}
          description={t('add_directory_desc')}
          action={
            <button
              onClick={addDirectory}
              className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded-lg hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
            >
              <PlusIcon className="size-4" />
              {t('add_first_directory')}
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
                    <span className="text-xs text-gray-400 dark:text-gray-500">{t('recursive')}</span>
                  )}
                  <button
                    type="button"
                    onClick={() => handleTogglePrivate(dir.id, !!dir.private)}
                    className={`ml-auto text-xs px-2 py-0.5 rounded transition-colors ${
                      dir.private
                        ? 'bg-red-100 dark:bg-red-900/30 text-red-700 dark:text-red-300'
                        : 'text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800'
                    }`}
                    title={dir.private ? t('private_dir_on') : t('private_dir_off')}
                  >
                    {dir.private ? t('private_dir') : t('public_dir')}
                  </button>
                </div>
                <p className="text-xs text-gray-500 dark:text-gray-400 truncate mt-0.5 font-mono">{dir.path}</p>
                <div className="flex items-center gap-3 mt-2 text-xs text-gray-400 dark:text-gray-500">
                  <span>{t('ocr_label', { lang: dir.ocr_lang })}</span>
                  {dir.include_exts && <span>{t('exts_label', { exts: dir.include_exts })}</span>}
                </div>
              </div>
              <button
                onClick={async () => {
                  const confirmed = await ask(t('confirm_remove_dir'), { title: t('remove_dir_title'), kind: 'warning' })
                  if (confirmed) {
                    removeDirectory(dir.id)
                  }
                }}
                className="p-1.5 rounded-md text-gray-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/20 transition-colors shrink-0"
                  title={t('remove_directory')}
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
