import { useEffect, useMemo, useState } from 'react'
import { useI18n } from '../i18n'
import { useSetup, type ProgressState } from '../hooks/useSetup'
import { LoadingSpinner } from '../icons'
import type { DepStatus } from '../api/settings'

interface SetupWizardProps {
  onDone: () => void
}

function fmtSize(bytes: number): string {
  if (bytes <= 0) return ''
  const mb = bytes / (1024 * 1024)
  if (mb < 1024) return `${mb.toFixed(0)} MB`
  return `${(mb / 1024).toFixed(1)} GB`
}

function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb.toFixed(0)} KB`
  const mb = kb / 1024
  if (mb < 1024) return `${mb.toFixed(1)} MB`
  return `${(mb / 1024).toFixed(2)} GB`
}

function progressPct(p: ProgressState | null, dep: DepStatus): number | null {
  if (!p || p.dep !== dep.id) return null
  // current file bytes / expected size is an upper bound; fall back to file
  // count fraction when size is unknown.
  if (dep.size_bytes > 0 && p.bytes > 0) {
    const frac = p.bytes / dep.size_bytes
    if (frac > 0 && frac < 1.2) return Math.min(100, Math.round(frac * 100))
  }
  if (p.total > 0) return Math.round(((p.current - 1) / p.total) * 100)
  return null
}

export default function SetupWizard({ onDone }: SetupWizardProps) {
  const { t } = useI18n()
  const setup = useSetup()
  const [dismissed, setDismissed] = useState(false)

  // Auto-close when all recommended deps are ready.
  useEffect(() => {
    if (!setup.loading && setup.status?.all_recommended_ready) {
      onDone()
    }
  }, [setup.loading, setup.status, onDone])

  const recommended = useMemo(
    () => (setup.status?.deps ?? []).filter(d => d.recommended),
    [setup.status],
  )
  const missing = useMemo(() => recommended.filter(d => !d.available), [recommended])
  const installingDep = setup.activeDep
  const doneCount = recommended.length - missing.length

  if (dismissed) return null

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-gray-900/60 backdrop-blur-sm p-4">
      <div className="bg-white dark:bg-gray-900 rounded-2xl shadow-2xl w-full max-w-lg overflow-hidden">
        <div className="p-6 border-b border-gray-200 dark:border-gray-800">
          <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">
            {t('setup_title')}
          </h2>
          <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
            {t('setup_desc')}
          </p>
        </div>

        <div className="p-6 space-y-3 max-h-[50vh] overflow-y-auto">
          {/* Summary */}
          <div className="text-sm text-gray-600 dark:text-gray-400">
            {t('setup_progress', { done: doneCount, total: recommended.length })}
          </div>

          {/* List */}
          {recommended.map(dep => {
            const installing = installingDep === dep.id
            const pct = progressPct(setup.progress, dep)
            return (
              <div
                key={dep.id}
                className={`flex items-start gap-3 p-3 rounded-lg border ${
                  dep.available
                    ? 'border-green-200 dark:border-green-900 bg-green-50/60 dark:bg-green-900/10'
                    : 'border-gray-200 dark:border-gray-700'
                }`}
              >
                <div className="mt-0.5 text-lg">
                  {dep.available ? (
                    <span className="text-green-600 dark:text-green-400">✓</span>
                  ) : installing ? (
                    <LoadingSpinner className="size-5 text-blue-500" />
                  ) : (
                    <span className="text-amber-500">○</span>
                  )}
                </div>
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium text-gray-900 dark:text-gray-100">{dep.name}</p>
                  <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                    {dep.hint}
                    {dep.size_bytes > 0 && ` · ${fmtSize(dep.size_bytes)}`}
                  </p>
                  {installing && pct !== null && (
                    <div className="mt-2">
                      <div className="h-1.5 w-full bg-gray-200 dark:bg-gray-700 rounded-full overflow-hidden">
                        <div className="h-full bg-blue-500 transition-all" style={{ width: `${pct}%` }} />
                      </div>
                      <p className="text-xs text-gray-500 mt-1">
                        {setup.progress && fmtBytes(setup.progress.bytes)} · {pct}%
                      </p>
                    </div>
                  )}
                  {!dep.available && setup.lastResult?.dep === dep.id && (
                    <p className={`text-xs mt-1 ${setup.lastResult.success ? 'text-green-600' : 'text-red-500'}`}>
                      {setup.lastResult.message}
                    </p>
                  )}
                </div>
                {!dep.available && (
                  <button
                    onClick={() => void setup.startInstall(dep.id)}
                    disabled={!!installingDep}
                    className="shrink-0 flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg transition-colors"
                  >
                    {installing && <LoadingSpinner className="size-3.5" />}
                    {installing ? t('installing') : t('install')}
                  </button>
                )}
              </div>
            )
          })}

          {missing.length === 0 && (
            <p className="text-center text-sm text-green-600 dark:text-green-400 py-4">
              {t('setup_all_ready')}
            </p>
          )}
        </div>

        <div className="px-6 py-4 border-t border-gray-200 dark:border-gray-800 flex items-center justify-between gap-3">
          <button
            onClick={() => {
              if (installingDep) {
                void setup.cancelInstall()
              } else {
                setDismissed(true)
                onDone()
              }
            }}
            className="px-4 py-2 text-sm text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 rounded-lg transition-colors"
          >
            {installingDep ? t('cancel') : t('setup_skip')}
          </button>
          <button
            onClick={() => {
              const next = missing.find(d => d.id !== installingDep)
              if (next) void setup.startInstall(next.id)
            }}
            disabled={missing.length === 0 || !!installingDep}
            className="px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg transition-colors"
          >
            {t('setup_install_all')}
          </button>
        </div>
      </div>
    </div>
  )
}
