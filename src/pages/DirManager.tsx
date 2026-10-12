import { useEffect, useState } from 'react'
import { useDirs } from '../hooks/useDirs'
import { addDir, type DirConfig } from '../api/dirs'
import { PlusIcon, TrashIcon, FolderIcon } from '../icons'
import { useI18n } from '../i18n'
import EmptyState from '../components/EmptyState'
import { ListSkeleton } from '../components/Skeleton'

type RemovePhase = 'confirm' | 'running' | 'done' | 'error'

export default function DirManager() {
  const { t } = useI18n()
  const { dirs, loading, error, addDirectory, removeDirectory, refresh } = useDirs()

  // Directory queued for removal. `confirm()` is a native OK/Cancel dialog with
  // no room for the "also wipe all data" checkbox, so removal uses this modal.
  const [pendingRemove, setPendingRemove] = useState<DirConfig | null>(null)
  const [purgeData, setPurgeData] = useState(false)
  const [phase, setPhase] = useState<RemovePhase>('confirm')
  const [result, setResult] = useState('')
  const [errText, setErrText] = useState('')
  const [elapsed, setElapsed] = useState(0)

  // The removal can legitimately take minutes (it waits for an in-flight scan to
  // stop, then re-indexes the remaining directories), so show a live counter
  // instead of a frozen dialog.
  useEffect(() => {
    if (phase !== 'running') return
    const id = setInterval(() => setElapsed(s => s + 1), 1000)
    return () => clearInterval(id)
  }, [phase])

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

  const openRemove = (dir: DirConfig) => {
    setPendingRemove(dir)
    setPurgeData(false)
    setPhase('confirm')
    setResult('')
    setErrText('')
    setElapsed(0)
  }

  const closeRemove = () => {
    // Never closable while the backend is working.
    if (phase === 'running') return
    setPendingRemove(null)
  }

  const handleRemove = async () => {
    if (!pendingRemove) return
    setPhase('running')
    setElapsed(0)
    setErrText('')
    setResult('')
    try {
      const message = await removeDirectory(pendingRemove.id, purgeData)
      setResult(message)
      setPhase('done')
    } catch (e) {
      setErrText(e instanceof Error ? e.message : String(e))
      setPhase('error')
    }
  }

  const mmss = `${Math.floor(elapsed / 60)}:${String(elapsed % 60).padStart(2, '0')}`

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
                </div>
                <p className="text-xs text-gray-500 dark:text-gray-400 truncate mt-0.5 font-mono">{dir.path}</p>
                <div className="flex items-center gap-3 mt-2 text-xs text-gray-400 dark:text-gray-500">
                  <span>{t('ocr_label', { lang: dir.ocr_lang })}</span>
                  {dir.include_exts && <span>{t('exts_label', { exts: dir.include_exts })}</span>}
                </div>
              </div>
              <button
                onClick={() => openRemove(dir)}
                className="p-1.5 rounded-md text-gray-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/20 transition-colors shrink-0"
                title={t('remove_directory')}
              >
                <TrashIcon className="size-4" />
              </button>
            </div>
          ))}
        </div>
      )}

      {pendingRemove && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
          onClick={closeRemove}
        >
          <div
            className="w-[460px] max-w-full rounded-xl bg-white dark:bg-gray-800 p-5 shadow-xl"
            onClick={e => e.stopPropagation()}
          >
            <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('remove_dir_title')}
            </h3>
            <p className="mt-1 text-xs font-mono text-gray-500 dark:text-gray-400 break-all">
              {pendingRemove.path}
            </p>

            {phase === 'confirm' && (
              <>
                <p className="mt-3 text-sm text-gray-600 dark:text-gray-300">
                  {t('confirm_remove_dir')}
                </p>
                <label className="mt-4 flex items-start gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={purgeData}
                    onChange={e => setPurgeData(e.target.checked)}
                    className="mt-0.5 size-4 shrink-0 accent-red-600"
                  />
                  <span className="text-sm text-gray-700 dark:text-gray-300">
                    {t('remove_dir_purge')}
                    <span className="block text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                      {t('remove_dir_purge_hint')}
                    </span>
                  </span>
                </label>
              </>
            )}

            {phase === 'running' && (
              <div className="mt-4 flex items-start gap-3">
                <span className="mt-0.5 size-4 shrink-0 animate-spin rounded-full border-2 border-gray-300 border-t-red-600" />
                <div className="text-sm text-gray-700 dark:text-gray-300">
                  <div>{t('remove_dir_running')}<span className="ml-2 font-mono text-xs text-gray-500">{mmss}</span></div>
                  <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">
                    {t('remove_dir_running_hint')}
                  </p>
                </div>
              </div>
            )}

            {phase === 'done' && (
              <div className="mt-4 rounded-lg bg-green-50 dark:bg-green-900/20 px-3 py-2 text-sm text-green-700 dark:text-green-300">
                {result || t('remove_dir_done')}
              </div>
            )}

            {phase === 'error' && (
              <div className="mt-4 rounded-lg bg-red-50 dark:bg-red-900/20 px-3 py-2 text-sm text-red-700 dark:text-red-300">
                <div className="font-medium">{t('remove_dir_failed')}</div>
                <div className="mt-1 break-all">{errText}</div>
              </div>
            )}

            <div className="mt-5 flex justify-end gap-2">
              {phase === 'confirm' && (
                <>
                  <button
                    onClick={closeRemove}
                    className="px-4 py-2 text-sm font-medium text-gray-600 dark:text-gray-300 bg-gray-100 dark:bg-gray-700 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors"
                  >
                    {t('cancel')}
                  </button>
                  <button
                    onClick={handleRemove}
                    className="px-4 py-2 text-sm font-medium text-white bg-red-600 hover:bg-red-700 rounded-lg transition-colors"
                  >
                    {t('remove_directory')}
                  </button>
                </>
              )}
              {(phase === 'done' || phase === 'error') && (
                <button
                  onClick={closeRemove}
                  className="px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors"
                >
                  {t('close')}
                </button>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
