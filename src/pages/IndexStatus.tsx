import { useEffect, useState } from 'react'
import { ask } from '@tauri-apps/plugin-dialog'
import { invoke } from '@tauri-apps/api/core'
import { useIndexStatus } from '../hooks/useIndexStatus'
import { getIndexErrors, type IndexError, type IndexStatus } from '../api/index'
import { getDuplicates, type DuplicateGroup } from '../api/files'
import { getFileTypeStats, type FileTypeStat } from '../api/search'
import { LoadingSpinner, RefreshIcon } from '../icons'
import { getSettings, listOcrEngines } from '../api/settings'
import EmptyState from '../components/EmptyState'
import { StatsCardSkeleton } from '../components/Skeleton'

function formatTime(ts: number | null): string {
  if (!ts) return 'Never'
  return new Date(ts / 1000).toLocaleString()
}

export default function IndexStatus() {
  const { status, loading, error, scan, rebuild } = useIndexStatus()
  const [duplicates, setDuplicates] = useState<DuplicateGroup[]>([])
  const [dupesLoading, setDupesLoading] = useState(false)
  const [rebuilding, setRebuilding] = useState(false)
  const [scanError, setScanError] = useState<string | null>(null)
  const [typeStats, setTypeStats] = useState<FileTypeStat[]>([])
  const [showErrors, setShowErrors] = useState(false)
  const [errorsList, setErrorsList] = useState<IndexError[]>([])
  const [retrying, setRetrying] = useState(false)
  const [lastDelta, setLastDelta] = useState<{ added: number; deleted: number; modified: number; errors: number } | null>(null)

  const handleScan = async () => {
    setScanError(null)
    const settings = await getSettings()
    const engineType = settings['ocr_engine']
    if (!engineType) {
      setScanError('请先在设置页面选择 OCR 引擎')
      return
    }
    const engines = await listOcrEngines()
    const selected = engines.find(e => e.engine_type === engineType)
    if (selected && !selected.available) {
      setScanError(`OCR 引擎 "${selected.name}" 不可用，请检查安装`)
      return
    }
    scan()
  }

  useEffect(() => {
    if (status?.total_files) {
      setDupesLoading(true)
      getDuplicates()
        .then(setDuplicates)
        .catch(() => {})
        .finally(() => setDupesLoading(false))
    }
  }, [status?.total_files])

  // Get file type distribution from backend
  useEffect(() => {
    getFileTypeStats()
      .then(setTypeStats)
      .catch(() => {})
  }, [])

  // Compute recent changes from scan_delta
  useEffect(() => {
    if (status?.scan_delta) {
      setLastDelta(status.scan_delta)
    } else {
      setLastDelta(null)
    }
  }, [status?.scan_delta])

  const handleRebuild = async () => {
    const confirmed = await ask(
        '重建索引将删除所有现有索引数据并重新扫描所有目录。此操作不可撤销，确定继续吗？',
        { title: '重建索引', kind: 'warning' }
    )
    if (!confirmed) return
    setRebuilding(true)
    await rebuild()
    setRebuilding(false)
  }

  const retryFailed = async () => {
    setRetrying(true)
    scan()
    setRetrying(false)
  }

  const handleShowErrors = async () => {
    if (showErrors) {
      setShowErrors(false)
      return
    }
    const errs = await getIndexErrors(50)
    setErrorsList(errs)
    setShowErrors(true)
  }

  const handleCancelScan = async () => {
    try {
      await invoke('cancel_scan')
    } catch (e) {
      console.error('Cancel scan failed:', e)
    }
  }

  const progress = status && status.total_files > 0
    ? Math.round((status.indexed / status.total_files) * 100)
    : 0

  return (
    <div className="h-full p-6 overflow-y-auto">
      <div className="flex items-center justify-between mb-6">
        <div>
          <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">Index Status</h2>
          <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
            Overview of the document index and scanning status
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleScan}
            disabled={status?.is_scanning}
            className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-gray-700 dark:text-gray-300 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-50 transition-colors"
          >
            <RefreshIcon className="size-4" />
            Scan Now
          </button>
          {status?.is_scanning && (
            <button
              onClick={handleCancelScan}
              className="px-3 py-2 text-sm font-medium text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-900 rounded-lg hover:bg-red-100 dark:hover:bg-red-900/40 transition-colors"
            >
              Cancel Scan
            </button>
          )}
          <button
            onClick={handleRebuild}
            disabled={rebuilding}
            className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-white bg-amber-600 hover:bg-amber-700 rounded-lg disabled:opacity-50 transition-colors"
          >
            {rebuilding && <LoadingSpinner className="size-4" />}
            Rebuild Index
          </button>
        </div>
      </div>

      {loading && (
        <div className="grid grid-cols-4 gap-4 mb-6">
          <StatsCardSkeleton />
          <StatsCardSkeleton />
          <StatsCardSkeleton />
          <StatsCardSkeleton />
        </div>
      )}

      {error && (
        <div className="px-4 py-3 mb-4 text-sm text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-900 rounded-lg">
          {error}
        </div>
      )}

      {scanError && (
        <div className="px-4 py-3 mb-4 text-sm text-amber-600 dark:text-amber-400 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-900 rounded-lg">
          {scanError}
        </div>
      )}

      {status && (
        <>
          <div className="grid grid-cols-5 gap-4 mb-6">
            <StatCard label="Total Files" value={status.total_files.toLocaleString()} color="gray" />
            <StatCard label="Indexed" value={status.indexed.toLocaleString()} color="green" />
            <StatCard label="Pending" value={status.pending.toLocaleString()} color="yellow" />
            <StatCard label="OCR'd" value={status.ocred.toLocaleString()} color="purple" />
            <div
              className={`p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg cursor-pointer hover:border-red-300 dark:hover:border-red-700 transition-colors ${status.errors > 0 ? 'cursor-pointer' : ''}`}
              onClick={status.errors > 0 ? handleShowErrors : undefined}
            >
              <p className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Errors</p>
              <p className={`text-2xl font-semibold ${status.errors > 0 ? 'text-red-600 dark:text-red-400' : 'text-gray-900 dark:text-gray-100'}`}>
                {status.errors.toLocaleString()}
              </p>
              {status.errors > 0 && (
                <div className="flex items-center gap-2 mt-2">
                  <button
                    onClick={(e) => { e.stopPropagation(); retryFailed() }}
                    disabled={retrying || status.is_scanning}
                    className="flex items-center gap-1 text-xs text-blue-600 dark:text-blue-400 hover:underline disabled:opacity-50"
                  >
                    {retrying ? <LoadingSpinner className="size-3" /> : <RefreshIcon className="size-3" />}
                    Retry
                  </button>
                  <span className="text-xs text-gray-400 dark:text-gray-500">|</span>
                  <span className="text-xs text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300" onClick={(e) => { e.stopPropagation(); handleShowErrors() }}>
                    Details
                  </span>
                </div>
              )}
            </div>
          </div>

          <div className="mb-6">
            <div className="flex items-center justify-between mb-2">
              <span className="text-xs font-medium text-gray-500 dark:text-gray-400">Index Progress</span>
              <span className="text-xs text-gray-500 dark:text-gray-400">{progress}%</span>
            </div>
            <div className="h-2 bg-gray-200 dark:bg-gray-800 rounded-full overflow-hidden">
              <div
                className="h-full bg-blue-600 rounded-full transition-all duration-500"
                style={{ width: `${progress}%` }}
              />
            </div>
          </div>

          {showErrors && errorsList.length > 0 && (
            <div className="mb-6 p-4 bg-white dark:bg-gray-900 border border-red-200 dark:border-red-900 rounded-lg">
              <div className="flex items-center justify-between mb-3">
                <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100">Error Details</h3>
                <button onClick={() => setShowErrors(false)} className="text-xs text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">Close</button>
              </div>
              <div className="max-h-64 overflow-y-auto space-y-2">
                {errorsList.map((err) => (
                  <div key={err.file_id} className="text-xs p-2 bg-red-50 dark:bg-red-900/10 border border-red-100 dark:border-red-900/20 rounded">
                    <div className="flex items-center justify-between mb-1">
                      <span className="font-medium text-red-700 dark:text-red-300">{err.error_type}</span>
                      <span className="text-gray-400">{new Date(err.created_at / 1000).toLocaleString()}</span>
                    </div>
                    <p className="text-gray-600 dark:text-gray-400 truncate" title={err.file_path}>{err.file_path}</p>
                    <p className="text-red-600 dark:text-red-400 mt-0.5">{err.error_msg}</p>
                  </div>
                ))}
                {errorsList.length === 0 && (
                  <p className="text-sm text-gray-400 dark:text-gray-500">No error details available.</p>
                )}
              </div>
            </div>
          )}

          <div className="grid grid-cols-2 gap-6">
            <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg">
              <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 mb-2">Scan Info</h3>
              <dl className="space-y-2 text-sm">
                <InfoRow label="Last scan" value={formatTime(status.last_scan)} />
                <InfoRow label="Status" value={status.is_scanning ? 'Scanning...' : 'Idle'} />
                <InfoRow label="Total indexed" value={`${status.indexed} / ${status.total_files}`} />
              </dl>
            </div>

            <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg">
              <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 mb-2">
                File Types
              </h3>
              {typeStats.length === 0 ? (
                <p className="text-sm text-gray-400 dark:text-gray-500">暂无数据</p>
              ) : (
                <div className="space-y-1.5">
                  {typeStats.map((t) => (
                    <div key={t.extension} className="flex items-center gap-2">
                      <span className="text-xs w-12 truncate font-medium text-gray-700 dark:text-gray-300">{t.name}</span>
                      <div className="flex-1 h-4 bg-gray-100 dark:bg-gray-800 rounded-full overflow-hidden">
                        {status?.total_files && status.total_files > 0 && (
                          <div className="h-full bg-blue-500 rounded-full transition-all" 
                                style={{ width: `${(t.count / status.total_files) * 100}%` }} 
                          />
                        )}
                      </div>
                      <span className="text-xs text-gray-500 w-10 text-right">{t.count}</span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>

          <div className="grid grid-cols-2 gap-6 mt-6">
            <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg">
              <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 mb-2">Recent Changes</h3>
              {!lastDelta ? (
                <p className="text-sm text-gray-400 dark:text-gray-500">暂无数据</p>
              ) : (
                <div className="space-y-2">
                  <DeltaRow label="Added" value={lastDelta.added} color="green" />
                  <DeltaRow label="Modified" value={lastDelta.modified} color="yellow" />
                  <DeltaRow label="Deleted" value={lastDelta.deleted} color="red" />
                  <DeltaRow label="Errors" value={lastDelta.errors} color="red" />
                </div>
              )}
            </div>

            <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg">
              <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 mb-2">
                Duplicates
                {dupesLoading && <LoadingSpinner className="size-3 ml-2 inline" />}
              </h3>
              {duplicates.length === 0 && !dupesLoading && (
                <EmptyState title="No duplicates found" />
              )}
              {duplicates.length > 0 && (
                <div className="space-y-2 max-h-48 overflow-y-auto">
                  {duplicates.map(d => (
                    <div key={d.md5} className="text-xs text-gray-600 dark:text-gray-400">
                      <span className="font-medium text-gray-900 dark:text-gray-100">{d.count}x</span> — {d.md5.slice(0, 12)}
                      <div className="text-gray-400 dark:text-gray-500 truncate">{d.paths[0]}</div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        </>
      )}
    </div>
  )
}

function StatCard({ label, value, color }: { label: string; value: string; color: 'gray' | 'green' | 'yellow' | 'red' | 'purple' }) {
  const colors: Record<string, string> = {
    gray: 'text-gray-900 dark:text-gray-100',
    green: 'text-green-600 dark:text-green-400',
    yellow: 'text-yellow-600 dark:text-yellow-400',
    red: 'text-red-600 dark:text-red-400',
    purple: 'text-purple-600 dark:text-purple-400',
  }
  return (
    <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg">
      <p className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{label}</p>
      <p className={`text-2xl font-semibold ${colors[color]}`}>{value}</p>
    </div>
  )
}

function InfoRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between">
      <span className="text-gray-500 dark:text-gray-400">{label}</span>
      <span className="text-gray-900 dark:text-gray-100 font-medium">{value}</span>
    </div>
  )
}

function DeltaRow({ label, value, color }: { label: string; value: number; color: 'green' | 'yellow' | 'red' }) {
  const colors: Record<string, string> = {
    green: 'text-green-600 dark:text-green-400',
    yellow: 'text-yellow-600 dark:text-yellow-400',
    red: 'text-red-600 dark:text-red-400',
  }
  return (
    <div className="flex items-center justify-between text-sm">
      <span className="text-gray-500 dark:text-gray-400">{label}</span>
      <span className={`font-medium ${colors[color]}`}>+{value}</span>
    </div>
  )
}
