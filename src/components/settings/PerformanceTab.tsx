import { useEffect, useState } from 'react'
import { useI18n } from '../../i18n'
import { LoadingSpinner } from '../../icons'
import { detectHardware, autoOptimize, getPerformanceProfile, type HardwareInfo, type PerformanceProfile } from '../../api/settings'
import { Section } from './SettingsFields'

const TIER_LABELS: Record<string, string> = { high: '⚡', balanced: '⚖️', conservative: '🐢' }
const TIER_COLORS: Record<string, string> = {
  high: 'text-green-600 dark:text-green-400',
  balanced: 'text-blue-600 dark:text-blue-400',
  conservative: 'text-amber-600 dark:text-amber-400',
}

export function PerformanceTab({ onFieldChange }: { onFieldChange: (key: string, value: string) => void }) {
  const { t } = useI18n()
  const [hw, setHw] = useState<HardwareInfo | null>(null)
  const [profile, setProfile] = useState<PerformanceProfile | null>(null)
  const [detecting, setDetecting] = useState(false)
  const [optimizing, setOptimizing] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const refresh = () => {
    setDetecting(true)
    setError(null)
    Promise.all([detectHardware(), getPerformanceProfile()])
      .then(([h, p]) => { setHw(h); setProfile(p) })
      .catch(e => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setDetecting(false))
  }

  useEffect(() => { refresh() }, [])

  const handleAutoOptimize = async () => {
    setOptimizing(true)
    setError(null)
    try {
      const p = await autoOptimize()
      setProfile(p)
      onFieldChange('perf_batch_io_concurrency', String(p.batch_io_concurrency))
      onFieldChange('perf_commit_interval', String(p.commit_interval))
      onFieldChange('perf_writer_buffer_mb', String(p.writer_buffer_mb))
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setOptimizing(false)
    }
  }

  return (
    <div className="space-y-6">
      <Section title={t('perf_hardware')}>
        {detecting ? (
          <div className="flex items-center gap-2 text-sm text-gray-500 dark:text-gray-400">
            <LoadingSpinner className="size-4" />
            {t('perf_detecting')}
          </div>
        ) : hw ? (
          <div className="grid grid-cols-2 gap-3 text-sm">
            <div className="p-3 bg-gray-50 dark:bg-gray-800 rounded-lg">
              <div className="text-gray-500 dark:text-gray-400 text-xs">{t('perf_cpu_cores')}</div>
              <div className="text-lg font-semibold text-gray-900 dark:text-gray-100">{hw.cpu_cores}</div>
            </div>
            <div className="p-3 bg-gray-50 dark:bg-gray-800 rounded-lg">
              <div className="text-gray-500 dark:text-gray-400 text-xs">{t('perf_disk_type')}</div>
              <div className="text-lg font-semibold text-gray-900 dark:text-gray-100">{hw.disk_type.toUpperCase()}</div>
            </div>
            <div className="p-3 bg-gray-50 dark:bg-gray-800 rounded-lg">
              <div className="text-gray-500 dark:text-gray-400 text-xs">{t('perf_disk_speed')}</div>
              <div className="text-lg font-semibold text-gray-900 dark:text-gray-100">{hw.disk_speed_mbps.toFixed(0)} MB/s</div>
            </div>
            <div className="p-3 bg-gray-50 dark:bg-gray-800 rounded-lg">
              <div className="text-gray-500 dark:text-gray-400 text-xs">{t('perf_platform')}</div>
              <div className="text-lg font-semibold text-gray-900 dark:text-gray-100">{hw.platform}</div>
            </div>
          </div>
        ) : null}
      </Section>

      <Section title={t('perf_current_profile')}>
        {profile ? (
          <div className="space-y-3">
            <div className="flex items-center gap-2">
              <span className="text-2xl">{TIER_LABELS[profile.tier] ?? '⚙️'}</span>
              <span className={`font-semibold ${TIER_COLORS[profile.tier] ?? ''}`}>
                {t(`perf_tier_${profile.tier}`)}
              </span>
            </div>
            <div className="grid grid-cols-3 gap-2 text-sm">
              <div className="p-2 bg-gray-50 dark:bg-gray-800 rounded text-center">
                <div className="text-gray-500 dark:text-gray-400 text-xs">{t('perf_concurrency')}</div>
                <div className="font-semibold text-gray-900 dark:text-gray-100">{profile.batch_io_concurrency}</div>
              </div>
              <div className="p-2 bg-gray-50 dark:bg-gray-800 rounded text-center">
                <div className="text-gray-500 dark:text-gray-400 text-xs">{t('perf_commit_interval')}</div>
                <div className="font-semibold text-gray-900 dark:text-gray-100">{profile.commit_interval}</div>
              </div>
              <div className="p-2 bg-gray-50 dark:bg-gray-800 rounded text-center">
                <div className="text-gray-500 dark:text-gray-400 text-xs">{t('perf_writer_buffer')}</div>
                <div className="font-semibold text-gray-900 dark:text-gray-100">{profile.writer_buffer_mb} MB</div>
              </div>
            </div>
          </div>
        ) : null}
      </Section>

      <Section title={t('perf_auto_optimize')}>
        <p className="text-sm text-gray-600 dark:text-gray-400 mb-3">
          {t('perf_auto_optimize_desc')}
        </p>
        <button
          onClick={handleAutoOptimize}
          disabled={optimizing}
          className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-blue-600 dark:bg-blue-500 rounded-lg hover:bg-blue-700 dark:hover:bg-blue-600 disabled:opacity-50 transition-colors"
        >
          {optimizing && <LoadingSpinner className="size-4" />}
          ⚡ {t('perf_one_click_optimize')}
        </button>
      </Section>

      {error && (
        <div className="px-4 py-3 text-sm text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-900 rounded-lg">
          {error}
        </div>
      )}
    </div>
  )
}
