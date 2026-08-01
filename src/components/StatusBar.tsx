import { useEffect, useState } from 'react'
import { useIndexStatus } from '../hooks/useIndexStatus'
import { LoadingSpinner } from '../icons'
import { listenScanProgress, type ScanProgress } from '../api/index'

export default function StatusBar() {
  const { status, loading, error } = useIndexStatus()
  const [scanProgress, setScanProgress] = useState<ScanProgress | null>(null)

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
          <span>Loading...</span>
        </div>
      ) : !status && !loading ? (
        <span>Ready</span>
      ) : status ? (
        <>
          <div className="flex items-center gap-4">
            <span>{status.total_files} files</span>
            <span className="text-green-600 dark:text-green-400">{status.indexed} indexed</span>
            {status.pending > 0 && <span className="text-yellow-600 dark:text-yellow-400">{status.pending} pending</span>}
            {status.errors > 0 && <span className="text-red-600 dark:text-red-400">{status.errors} errors</span>}
            {status.ocred > 0
    ? <span className="text-purple-600 dark:text-purple-400">{status.ocred}/{status.total_images} OCR'd</span>
    : status.total_images > 0
    ? <span className="text-purple-600 dark:text-purple-400">0/{status.total_images} OCR'd</span>
    : null
}
          </div>
          <div className="flex items-center gap-2">
            {status.is_scanning && (
              scanProgress ? (
                <span className="flex items-center gap-1 text-blue-600 dark:text-blue-400">
                  <LoadingSpinner className="size-3" />
                  {scanProgress.phase === 'index' ? 'Indexing' : 'Scanning'}: {scanProgress.processed}/{scanProgress.total > 0 ? scanProgress.total : '?'}
                </span>
              ) : (
                <span className="flex items-center gap-1 text-blue-600 dark:text-blue-400">
                  <LoadingSpinner className="size-3" />
                  Scanning...
                </span>
              )
            )}
            {status.last_scan && (
               <span>
                 Last scan: {new Date(status.last_scan / 1000).toLocaleTimeString()}
               </span>
            )}
          </div>
        </>
      ) : null}
    </footer>
  )
}
