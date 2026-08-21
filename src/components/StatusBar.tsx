import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useIndexStatus } from '../hooks/useIndexStatus'
import { useI18n } from '../i18n'
import { LoadingSpinner } from '../icons'
import { listenScanProgress, type ScanProgress } from '../api/index'
import { getBackupStatus, type BackupStatus } from '../api/backup'

const LAST_READ_KEY = 'last_read_brief_ts'

export default function StatusBar() {
  const { t } = useI18n()
  const navigate = useNavigate()
  const { status, loading, error } = useIndexStatus()
  const [scanProgress, setScanProgress] = useState<ScanProgress | null>(null)
  const [hasUnreadBrief, setHasUnreadBrief] = useState(false)
  const [backupStatus, setBackupStatus] = useState<BackupStatus | null>(null)

  // Light the brief icon when a NEW task brief arrives (newest timestamp > the
  // last one the user acknowledged). Survives page switches (persisted).
  useEffect(() => {
    const briefs = status?.briefs ?? []
    if (briefs.length === 0) return
    const lastRead = parseInt(localStorage.getItem(LAST_READ_KEY) ?? '0', 10)
    if (briefs[0].completed_at > lastRead) {
      setHasUnreadBrief(true)
    }
  }, [status?.briefs])

  const handleBriefClick = () => {
    const briefs = status?.briefs ?? []
    if (briefs.length > 0) {
      localStorage.setItem(LAST_READ_KEY, String(briefs[0].completed_at))
    }
    setHasUnreadBrief(false)
    // Jump to the log viewer paused on the brief's task lines.
    const q = briefs[0]?.task ?? ''
    navigate(q ? `/logs?q=${encodeURIComponent(`[TASK] ${q}`)}` : '/logs')
  }

  useEffect(() => {
    getBackupStatus().then(setBackupStatus).catch(() => {})
    const id = setInterval(() => {
      getBackupStatus().then(setBackupStatus).catch(() => {})
    }, 60_000)
    return () => clearInterval(id)
  }, [])

  useEffect(() => {
    const unlisten = listenScanProgress(setScanProgress)
    return () => { unlisten.then(f => f()) }
  }, [])

  // Reset scan progress when scanning stops
  useEffect(() => {
    if (!status?.is_scanning) {
      setScanProgress(null)
    }
  }, [status?.is_scanning])

  return (
    <footer className="flex items-center justify-between px-4 py-1.5 border-t border-gray-200 dark:border-gray-800 bg-gray-50 dark:bg-gray-900 text-xs text-gray-500 dark:text-gray-400 shrink-0">
      {error && !loading ? (
        <span className="text-red-600 dark:text-red-400">{error}</span>
      ) : !status && loading ? (
        <div className="flex items-center gap-2">
          <LoadingSpinner className="size-3" />
          <span>{t('loading')}</span>
        </div>
      ) : !status && !loading ? (
        <span>{t('ready')}</span>
      ) : status ? (
        <>
          <div className="flex items-center gap-4">
            <span>{status.total_files} {t('files')}</span>
            <span className="text-green-600 dark:text-green-400">{status.indexed} {t('indexed')}</span>
            {status.pending > 0 && <span className="text-yellow-600 dark:text-yellow-400">{status.pending} {t('pending')}</span>}
            {status.errors > 0 && <span className="text-red-600 dark:text-red-400">{status.errors} {t('errors')}</span>}
            {status.ocred > 0
    ? <span className="text-purple-600 dark:text-purple-400">{t('ocrd_count', { done: status.ocred, total: status.total_images })}</span>
    : status.total_images > 0
    ? <span className="text-purple-600 dark:text-purple-400">{t('ocrd_count', { done: 0, total: status.total_images })}</span>
    : null
}
          </div>
          <div className="flex items-center gap-2">
            {status.is_scanning && (
              scanProgress ? (
                <span className="flex items-center gap-1 text-blue-600 dark:text-blue-400">
                  <LoadingSpinner className="size-3" />
                  {scanProgress.phase === 'index' ? t('indexing') : t('scanning')}: {scanProgress.processed}/{scanProgress.total > 0 ? scanProgress.total : '?'}
                </span>
              ) : (
                <span className="flex items-center gap-1 text-blue-600 dark:text-blue-400">
                  <LoadingSpinner className="size-3" />
                  {t('scanning')}...
                </span>
              )
            )}
            {hasUnreadBrief && (
              <button
                onClick={handleBriefClick}
                title={t('task_brief_unread', { task: status?.briefs?.[0]?.task ?? '' })}
                className="relative flex items-center gap-1 px-1.5 py-0.5 rounded text-amber-600 dark:text-amber-400 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 hover:bg-amber-100 dark:hover:bg-amber-900/40 transition-colors"
              >
                <span className="animate-pulse">📋</span>
                {status?.briefs?.[0]?.summary}
              </button>
            )}
{status.last_scan && (
               <span>
                  {t('last_scan')}: {new Date(status.last_scan / 1000).toLocaleTimeString()}
               </span>
             )}
            {backupStatus && backupStatus.last_backup && (
              <span className="text-gray-400 dark:text-gray-500">
                💾 {t('last_backup')}: {new Date(backupStatus.last_backup * 1000).toLocaleTimeString()}
              </span>
            )}
          </div>
        </>
      ) : null}
    </footer>
  )
}
