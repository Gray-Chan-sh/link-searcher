import { useEffect, useState } from 'react'
import { invoke, listen } from '../api/client'
import { useNavigate } from 'react-router-dom'
import { useIndexStatus } from '../hooks/useIndexStatus'
import { getIndexErrors, backfillEmbeddings, verifyIndexContent, reextractMissingContent, listenScanProgress, type IndexError } from '../api/index'
import { getDuplicates, aiCapabilities, type DuplicateGroup } from '../api/files'
import { getFileTypeStats, type FileTypeStat } from '../api/search'
import { LoadingSpinner, RefreshIcon } from '../icons'
import { getSettings, listOcrEngines } from '../api/settings'
import { useI18n } from '../i18n'
import EmptyState from '../components/EmptyState'
import { StatsCardSkeleton } from '../components/Skeleton'

function formatTime(ts: number | null, t: (k: string) => string): string {
  if (!ts) return t('never')
  return new Date(ts / 1000).toLocaleString()
}

export default function IndexStatus() {
  const { t } = useI18n()
  const navigate = useNavigate()
  const { status, loading, error, scan, rebuild } = useIndexStatus()
  const [duplicates, setDuplicates] = useState<DuplicateGroup[]>([])
  const [dupesLoading, setDupesLoading] = useState(false)
  const [rebuilding, setRebuilding] = useState(false)
  const [forceDead, setForceDead] = useState(false)
  const [embedCapable, setEmbedCapable] = useState(false)
  const [backfillMsg, setBackfillMsg] = useState<string | null>(null)
  const [scanError, setScanError] = useState<string | null>(null)
  const [typeStats, setTypeStats] = useState<FileTypeStat[]>([])
  const [showErrors, setShowErrors] = useState(false)
  const [errorsList, setErrorsList] = useState<IndexError[]>([])
  const [retrying, setRetrying] = useState(false)
  const [lastDelta, setLastDelta] = useState<{ added: number; deleted: number; modified: number; errors: number } | null>(null)
  const [scanPhase, setScanPhase] = useState<string | null>(null)

  useEffect(() => {
    aiCapabilities().then(c => setEmbedCapable(c.embedding)).catch(() => {})
  }, [])

  useEffect(() => {
    const unlisten = listenScanProgress(p => setScanPhase(p.phase ?? null))
    return () => { unlisten.then(f => f()) }
  }, [])

  useEffect(() => {
    if (!status?.is_scanning) setScanPhase(null)
  }, [status?.is_scanning])

  const handleScan = async () => {
    setScanError(null)
    const settings = await getSettings()
    const engineType = settings['ocr_engine']
    if (!engineType) {
      setScanError(t('scan_error_no_engine'))
      return
    }
    const engines = await listOcrEngines()
    const selected = engines.find(e => e.engine_type === engineType)
    if (selected && !selected.available) {
      setScanError(t('scan_error_engine_unavailable', { name: selected.name }))
      return
    }
    scan()
  }

  useEffect(() => {
    const unlisten = listen('scan-completed', () => {
      setDupesLoading(true)
      getDuplicates()
        .then(setDuplicates)
        .catch(() => {})
        .finally(() => setDupesLoading(false))
    })
    return () => { unlisten.then(f => f()) }
  }, [])

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
    const confirmed = await confirm(t('confirm_rebuild'))
    if (!confirmed) return
    setRebuilding(true)
    await rebuild()
    setRebuilding(false)
  }

  const handleBackfill = async () => {
    setBackfillMsg(null)
    try {
      const r = await backfillEmbeddings()
      setBackfillMsg(
        r.processed > 0
          ? `✓ ${r.processed} 个已补齐${r.failed > 0 ? `，${r.failed} 个失败` : ''}`
          : '✓ 无缺失向量'
      )
    } catch (e) {
      setBackfillMsg(String(e))
    }
  }

  const handleVerify = async () => {
    setBackfillMsg(null)
    try {
      const r = await verifyIndexContent(forceDead)
      setBackfillMsg(
        t('index_verify_done', { checked: r.checked, recovered: r.recovered, dead: r.dead, failed: r.failed })
      )
    } catch (e) {
      setBackfillMsg(String(e))
    }
  }

  const handleReextract = async () => {
    setBackfillMsg(null)
    try {
      const r = await reextractMissingContent()
      setBackfillMsg(
        r.processed > 0
          ? t('index_reextract_done', { ok: r.ok, failedSuffix: r.failed > 0 ? `，${r.failed} 个失败` : '' })
          : t('index_no_missing')
      )
    } catch (e) {
      setBackfillMsg(String(e))
    }
  }

  const retryFailed = async () => {
    setRetrying(true)
    scan()
    setRetrying(false)
  }

  const taskActive = (name: string) => (status?.running_tasks ?? []).includes(name)

  const handleShowErrors = async () => {
    if (showErrors) {
      setShowErrors(false)
      return
    }
    try {
      const errs = await getIndexErrors(50)
      setErrorsList(errs)
      setShowErrors(true)
    } catch {
      setShowErrors(false)
    }
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
          <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">{t('index_status')}</h2>
          <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
            {t('index_status_overview')}
          </p>
        </div>
      <div className="flex items-center gap-2">
          <button
            onClick={handleScan}
            disabled={status?.is_scanning}
            className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-gray-700 dark:text-gray-300 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-50 transition-colors"
          >
            <RefreshIcon className="size-4" />
            {t('scan_now')}
          </button>
          {status?.is_scanning && (
            <button
              onClick={handleCancelScan}
              className="px-3 py-2 text-sm font-medium text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-900 rounded-lg hover:bg-red-100 dark:hover:bg-red-900/40 transition-colors"
            >
              {t('cancel_scan')}
            </button>
          )}
          <button
            onClick={handleBackfill}
            disabled={taskActive('backfill') || !embedCapable || status?.is_scanning}
            title={embedCapable ? '补齐缺失的语义向量（不重新提取/OCR）' : 'AI Embedding 网关未配置'}
            className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-purple-700 dark:text-purple-300 bg-purple-50 dark:bg-purple-900/20 border border-purple-200 dark:border-purple-800 rounded-lg hover:bg-purple-100 dark:hover:bg-purple-900/40 disabled:opacity-50 transition-colors"
          >
            {taskActive('backfill') && <LoadingSpinner className="size-4" />}
            ✦ 补齐语义向量
          </button>
          <button
            onClick={handleReextract}
            disabled={taskActive('reextract') || status?.is_scanning}
            title="重新提取缺失内容的文件（如旧版 .doc 扫描件，批量修复）"
            className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-gray-700 dark:text-gray-300 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-50 transition-colors"
          >
            {taskActive('reextract') && <LoadingSpinner className="size-4" />}
            ↻ 重提取缺失内容
          </button>
          <label className="flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400 cursor-pointer select-none" title="已确认空内容、被自动跳过验证的文件">
            <input
              type="checkbox"
              checked={forceDead}
              onChange={e => setForceDead(e.target.checked)}
              className="accent-blue-600"
            />
            含已标记文件
          </label>
          <button
            onClick={handleVerify}
            disabled={taskActive('verify') || status?.is_scanning}
            title="验证索引内容有效性：内容为空的已索引文件将自动重试一次"
            className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-teal-700 dark:text-teal-300 bg-teal-50 dark:bg-teal-900/20 border border-teal-200 dark:border-teal-800 rounded-lg hover:bg-teal-100 dark:hover:bg-teal-900/40 disabled:opacity-50 transition-colors"
          >
            {taskActive('verify') && <LoadingSpinner className="size-4" />}
            ✓ 验证索引有效性
          </button>
          <button
            onClick={handleRebuild}
            disabled={rebuilding}
            className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-white bg-amber-600 hover:bg-amber-700 rounded-lg disabled:opacity-50 transition-colors"
          >
            {rebuilding && <LoadingSpinner className="size-4" />}
            {t('rebuild_index')}
          </button>
        </div>
      </div>

      {backfillMsg && (
        <div className="mb-4 px-4 py-3 text-sm text-purple-700 dark:text-purple-300 bg-purple-50 dark:bg-purple-900/20 border border-purple-200 dark:border-purple-800 rounded-lg">
          {backfillMsg}
        </div>
      )}

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
            <div onClick={() => navigate('/browse')} className="cursor-pointer hover:opacity-80"><StatCard label={t('total_files')} value={status.total_files.toLocaleString()} color="gray" /></div>
            <div onClick={() => navigate('/browse?filter=indexed')} className="cursor-pointer hover:opacity-80"><StatCard label={t('indexed')} value={status.indexed.toLocaleString()} color="green" /></div>
            <div onClick={() => navigate('/browse?filter=pending')} className="cursor-pointer hover:opacity-80"><StatCard label={t('pending')} value={status.pending.toLocaleString()} color="yellow" subtitle={status.errors > 0 ? t('incl_errors') : undefined} /></div>
            <div onClick={() => navigate('/browse')} className="cursor-pointer hover:opacity-80"><StatCard label={t('ocred')} value={status.ocred.toLocaleString()} color="purple" /></div>
            <div
              className={`p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg cursor-pointer hover:border-red-300 dark:hover:border-red-700 transition-colors ${status.errors > 0 ? 'cursor-pointer' : ''}`}
              onClick={status.errors > 0 ? handleShowErrors : undefined}
            >
              <p className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{t('errors')}</p>
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
                   {t('retry')}
                 </button>
                  <span className="text-xs text-gray-400 dark:text-gray-500">|</span>
                  <span className="text-xs text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300" onClick={(e) => { e.stopPropagation(); handleShowErrors() }}>
                    {t('details')}
                  </span>
                </div>
              )}
            </div>
          </div>

          <div className="mb-6">
            <div className="flex items-center justify-between mb-2">
              <span className="text-xs font-medium text-gray-500 dark:text-gray-400">
                {t('index_progress')}
                {scanPhase && ` — ${scanPhase === 'index' ? `${t('indexing')}...` : `${t('scanning')}...`}`}
              </span>
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
                <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100">{t('error_details')}</h3>
                <button onClick={() => setShowErrors(false)} className="text-xs text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">{t('close')}</button>
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
                  <p className="text-sm text-gray-400 dark:text-gray-500">{t('no_error_details')}</p>
                )}
              </div>
            </div>
          )}

          <div className="grid grid-cols-2 gap-6">
            <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg">
              <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 mb-2">Scan Info</h3>
              <dl className="space-y-2 text-sm">
                <InfoRow label={t('last_scan')} value={formatTime(status.last_scan, t)} />
                <InfoRow label={t('status')} value={status.is_scanning ? `${t('scanning')}...` : t('idle')} />
                <InfoRow label={t('total_indexed')} value={`${status.indexed} / ${status.total_files}`} />
              </dl>
            </div>

            <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg">
              <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 mb-2">
                {t('file_types')}
              </h3>
              {typeStats.length === 0 ? (
                <p className="text-sm text-gray-400 dark:text-gray-500">{t('no_data')}</p>
              ) : (
                <div className="space-y-1.5">
                  {typeStats.map((t) => (
                    <div key={t.extension} className="flex items-center gap-2">
                      <span className="text-xs w-12 truncate font-medium text-gray-700 dark:text-gray-300">{t.name}</span>
                      <div className="flex-1 h-4 bg-gray-100 dark:bg-gray-800 rounded-full overflow-hidden flex">
                        {status?.total_files && status.total_files > 0 && (
                          <>
                            <div className="h-full bg-green-500 transition-all" style={{ width: `${(t.indexed / status.total_files) * 100}%` }} />
                            <div className="h-full bg-yellow-400 transition-all" style={{ width: `${(t.pending / status.total_files) * 100}%` }} />
                            <div className="h-full bg-red-400 transition-all" style={{ width: `${(t.failed / status.total_files) * 100}%` }} />
                          </>
                        )}
                      </div>
                      <span className="text-xs text-gray-500 w-16 text-right tabular-nums">
                        <span className="text-green-600">{t.indexed ?? 0}</span>
                        {t.pending > 0 && <span className="text-yellow-600"> {t.pending}</span>}
                        {t.failed > 0 && <span className="text-red-500"> {t.failed}</span>}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>

          <div className="grid grid-cols-2 gap-6 mt-6">
            <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg">
              <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 mb-2">{t('recent_changes')}</h3>
              {!lastDelta ? (
                <p className="text-sm text-gray-400 dark:text-gray-500">{t('no_data')}</p>
              ) : (
                <div className="space-y-2">
                  <DeltaRow label={t('added')} value={lastDelta.added} color="green" />
                  <DeltaRow label={t('modified')} value={lastDelta.modified} color="yellow" />
                  <DeltaRow label={t('deleted')} value={lastDelta.deleted} color="red" />
                  <DeltaRow label={t('errors')} value={lastDelta.errors} color="red" />
                </div>
              )}
            </div>

            <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg">
              <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 mb-2">
                {t('duplicates')}
                {dupesLoading && <LoadingSpinner className="size-3 ml-2 inline" />}
              </h3>
              {duplicates.length === 0 && !dupesLoading && (
                <EmptyState title={t('no_duplicates')} />
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

function StatCard({ label, value, color, subtitle }: { label: string; value: string; color: 'gray' | 'green' | 'yellow' | 'red' | 'purple'; subtitle?: string }) {
  const colors: Record<string, string> = {
    gray: 'text-gray-900 dark:text-gray-100',
    green: 'text-green-600 dark:text-green-400',
    yellow: 'text-yellow-600 dark:text-yellow-400',
    red: 'text-red-600 dark:text-red-400',
    purple: 'text-purple-600 dark:text-purple-400',
  }
  return (
    <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg transition-colors">
      <p className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{label}</p>
      <p className={`text-2xl font-semibold ${colors[color]}`}>{value}</p>
      {subtitle && <p className="text-xs text-gray-400 dark:text-gray-500 mt-0.5">{subtitle}</p>}
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
