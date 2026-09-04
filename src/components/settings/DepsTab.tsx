import { useMemo } from 'react'
import { useI18n } from '../../i18n'
import { useSetup } from '../../hooks/useSetup'
import { LoadingSpinner } from '../../icons'
import { Section } from './SettingsFields'

function fmtSize(bytes: number): string {
  if (bytes <= 0) return ''
  const mb = bytes / (1024 * 1024)
  return mb < 1024 ? `${mb.toFixed(0)} MB` : `${(mb / 1024).toFixed(1)} GB`
}

function fmtBytes(bytes: number): string {
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

/** Dependency center: full list of installable runtime deps with progress. */
export function DepsTab() {
  const { t } = useI18n()
  const setup = useSetup()

  const deps = useMemo(() => setup.status?.deps ?? [], [setup.status])
  if (setup.loading) {
    return (
      <div className="flex items-center justify-center h-40">
        <LoadingSpinner className="size-6 text-blue-500" />
      </div>
    )
  }

  const active = setup.activeDep

  return (
    <div className="space-y-6">
      <Section title={t('dep_center_title')}>
        <p className="text-sm text-gray-500 dark:text-gray-400 mb-3">{t('dep_center_desc')}</p>
        <p className="text-xs text-gray-400 mb-4">data dir: {setup.status?.data_dir}</p>

        <div className="space-y-2">
          {deps.map(dep => {
            const installing = active === dep.id
            const pct =
              active === dep.id && setup.progress && dep.size_bytes > 0
                ? Math.min(100, Math.round((setup.progress.bytes / dep.size_bytes) * 100))
                : null
            return (
              <div key={dep.id} className="flex items-start gap-3 p-3 rounded-lg border border-gray-200 dark:border-gray-700">
                <div className="mt-0.5 text-lg">
                  {dep.available ? (
                    <span className="text-green-600 dark:text-green-400">✓</span>
                  ) : installing ? (
                    <LoadingSpinner className="size-5 text-blue-500" />
                  ) : (
                    <span className="text-amber-500">✗</span>
                  )}
                </div>
                <div className="flex-1 min-w-0">
                  <p className="text-sm text-gray-900 dark:text-gray-100">
                    {dep.name}
                    {dep.recommended && <span className="ml-2 text-xs text-blue-500">推荐</span>}
                  </p>
                  <p className="text-xs text-gray-500 mt-0.5">
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
                  {!dep.available && !installing && setup.lastResult?.dep === dep.id && (
                    <p className={`text-xs mt-1 ${setup.lastResult.success ? 'text-green-600' : 'text-red-500'}`}>
                      {setup.lastResult.message}
                    </p>
                  )}
                </div>
                {!dep.available && (
                  <button
                    onClick={() => {
                      if (installing) void setup.cancelInstall()
                      else void setup.startInstall(dep.id)
                    }}
                    disabled={!!active && !installing}
                    className="shrink-0 px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg transition-colors"
                  >
                    {installing ? t('cancel') : dep.size_bytes > 0 ? t('install_now') : t('install_now')}
                  </button>
                )}
              </div>
            )
          })}
        </div>
      </Section>
    </div>
  )
}
