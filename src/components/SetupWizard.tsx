import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useI18n } from '../i18n'
import { useSetup, type ProgressState } from '../hooks/useSetup'
import { LoadingSpinner } from '../icons'
import type { DepStatus } from '../api/settings'
import { isTauri } from '../utils/platform'

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
  if (dep.size_bytes > 0 && p.bytes > 0) {
    const frac = p.bytes / dep.size_bytes
    if (frac > 0 && frac < 1.2) return Math.min(100, Math.round(frac * 100))
  }
  if (p.total > 0) return Math.round(((p.current - 1) / p.total) * 100)
  return null
}

const FEATURES = [
  { icon: '📁', titleKey: 'ob_step1_title', descKey: 'ob_step1_desc' },
  { icon: '🔍', titleKey: 'ob_step2_title', descKey: 'ob_step2_desc' },
  { icon: '🎉', titleKey: 'ob_step3_title', descKey: 'ob_step3_desc' },
] as const

/** Unified first-run wizard: feature intro → recommended deps → get started. */
export default function SetupWizard({ onDone }: SetupWizardProps) {
  const { t } = useI18n()
  const navigate = useNavigate()
  const setup = useSetup()
  const [step, setStep] = useState(0)

  // The three BGE models are one single-choice group: if any dimension is
  // installed, the recommended BGE requirement is satisfied.
  const BGE_IDS = ['bge-small', 'bge-base', 'bge-large']
  const deps = setup.status?.deps ?? []
  const anyBge = deps.some(d => BGE_IDS.includes(d.id) && d.available)
  const isAvailable = (d: DepStatus) => d.available || (BGE_IDS.includes(d.id) && anyBge)
  const recommended = deps.filter(d => d.recommended)
  const missing = recommended.filter(d => !isAvailable(d))
  const installingDep = setup.activeDep
  const doneCount = recommended.length - missing.length
  const depsReady = !setup.loading && recommended.length > 0 && missing.length === 0

  useEffect(() => {
    if (step === 1 && depsReady) setStep(2)
  }, [step, depsReady])

  const finish = () => onDone()

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-gray-900/60 backdrop-blur-sm p-4">
      <div className="bg-white dark:bg-gray-900 rounded-2xl shadow-2xl w-full max-w-lg overflow-hidden">
        {/* Header + step indicator */}
        <div className="p-6 border-b border-gray-200 dark:border-gray-800">
          <div className="flex gap-1.5 mb-4">
            {[0, 1, 2].map(i => (
              <div
                key={i}
                className={`h-1 flex-1 rounded-full ${i <= step ? 'bg-blue-600' : 'bg-gray-200 dark:bg-gray-700'}`}
              />
            ))}
          </div>
          <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">
            {step === 0 ? t('wizard_welcome_title') : step === 1 ? t('setup_title') : t('wizard_done_title')}
          </h2>
          <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
            {step === 0 ? t('wizard_welcome_desc') : step === 1 ? t('setup_desc') : t('wizard_done_desc')}
          </p>
        </div>

        <div className="p-6 space-y-4 max-h-[50vh] overflow-y-auto">
          {/* Step 0: feature intro */}
          {step === 0 && (
            <div className="space-y-3">
              {FEATURES.map(f => (
                <div
                  key={f.titleKey}
                  className="flex items-start gap-4 p-4 rounded-xl border border-gray-200 dark:border-gray-700"
                >
                  <span className="text-3xl leading-none mt-0.5">{f.icon}</span>
                  <div>
                    <p className="text-sm font-medium text-gray-900 dark:text-gray-100">{t(f.titleKey)}</p>
                    <p className="text-xs text-gray-500 dark:text-gray-400 mt-1">{t(f.descKey)}</p>
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* Step 1: recommended deps */}
          {step === 1 && (
            <div className="space-y-3">
              <div className="text-sm text-gray-600 dark:text-gray-400">
                {t('setup_progress', { done: doneCount, total: recommended.length })}
              </div>

              {recommended.map(dep => {
                const installing = installingDep === dep.id
                const pct = progressPct(setup.progress, dep)
                const ok = isAvailable(dep)
                return (
                  <div
                    key={dep.id}
                    className={`flex items-start gap-3 p-3 rounded-lg border ${
                      ok
                        ? 'border-green-200 dark:border-green-900 bg-green-50/60 dark:bg-green-900/10'
                        : 'border-gray-200 dark:border-gray-700'
                    }`}
                  >
                    <div className="mt-0.5 text-lg">
                      {ok ? (
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
                      {!ok && setup.lastResult?.dep === dep.id && (
                        <p className={`text-xs mt-1 ${setup.lastResult.success ? 'text-green-600' : 'text-red-500'}`}>
                          {setup.lastResult.message}
                        </p>
                      )}
                    </div>
                    {!ok && (
                      <button
                        onClick={() => void setup.startInstall(dep.id)}
                        disabled={!!installingDep}
                        className="shrink-0 flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg transition-colors"
                      >
                        {installing && <LoadingSpinner className="size-3.5" />}
                        {installing ? t('installing') : t('install_now')}
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
          )}

          {/* Step 2: getting started */}
          {step === 2 && (
            <div className="space-y-3">
              <div className="text-center py-4">
                <div className="text-5xl mb-3">🚀</div>
                <p className="text-sm text-gray-600 dark:text-gray-400">
                  {isTauri() && missing.length > 0
                    ? t('wizard_done_partial')
                    : t('setup_all_ready')}
                </p>
              </div>
              <button
                onClick={() => { finish(); navigate('/directories') }}
                className="w-full flex items-center justify-center gap-2 px-4 py-2.5 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors"
              >
                📁 {t('ob_step1_action')}
              </button>
              <button
                onClick={() => { finish(); navigate('/') }}
                className="w-full flex items-center justify-center gap-2 px-4 py-2.5 text-sm font-medium text-gray-700 dark:text-gray-200 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 rounded-lg transition-colors"
              >
                🔍 {t('ob_step3_action')}
              </button>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="px-6 py-4 border-t border-gray-200 dark:border-gray-800 flex items-center justify-between gap-3">
          {step === 0 ? (
            <button
              onClick={finish}
              className="px-4 py-2 text-sm text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 rounded-lg transition-colors"
            >
              {t('skip')}
            </button>
          ) : (
            <button
              onClick={() => setStep(s => s - 1)}
              className="px-4 py-2 text-sm text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 rounded-lg transition-colors"
            >
              {t('back')}
            </button>
          )}

          {step === 0 && (
            <button
              onClick={() => setStep(1)}
              className="px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors"
            >
              {t('next')}
            </button>
          )}

          {step === 1 && (
            <div className="flex items-center gap-2">
              {missing.length > 0 && (
                <button
                  onClick={() => {
                    const next = missing.find(d => d.id !== installingDep)
                    if (next) void setup.startInstall(next.id)
                  }}
                  disabled={!!installingDep}
                  className="px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg transition-colors"
                >
                  {t('setup_install_all')}
                </button>
              )}
              <button
                onClick={() => setStep(2)}
                className="px-4 py-2 text-sm font-medium text-gray-700 dark:text-gray-200 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 rounded-lg transition-colors"
              >
                {missing.length === 0 ? t('next') : t('skip')}
              </button>
            </div>
          )}

          {step === 2 && (
            <button
              onClick={finish}
              className="px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors"
            >
              {t('done')}
            </button>
          )}
        </div>
      </div>
    </div>
  )
}