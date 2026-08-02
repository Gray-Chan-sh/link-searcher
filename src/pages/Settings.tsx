import { useEffect, useRef, useState } from 'react'
import { ask, open, message } from '@tauri-apps/plugin-dialog'
import { listen } from '@tauri-apps/api/event'
import { useSettings } from '../hooks/useSettings'
import { useTheme } from '../theme'
import { useI18n } from '../i18n'
import { LoadingSpinner } from '../icons'
import { getConfig, migrateData, restartApp, updateConfig, type ConfigInfo, type MigrationProgress, type MigrationWarning } from '../api/config'
import { checkDependencies, listOcrEngines, testOcrEngine, updateSettings, type DependencyStatus, type OcrEngineStatus, type OcrTestResult } from '../api/settings'

const OCR_LANGS = [
  { value: 'eng', label: 'English' },
  { value: 'chi_sim', label: 'Chinese (Simplified)' },
  { value: 'jpn', label: 'Japanese' },
  { value: 'kor', label: 'Korean' },
]

const LANG_OPTIONS = [
  { value: 'zh', labelKey: 'chinese' },
  { value: 'en', labelKey: 'english' },
]

export default function Settings() {
  const { t, lang, setLang } = useI18n()
  const { settings, loading, error, setValue } = useSettings()
  const { theme, setTheme } = useTheme()
  const [ocrEngines, setOcrEngines] = useState<OcrEngineStatus[]>([])
  const [ocrTesting, setOcrTesting] = useState(false)
  const [ocrResult, setOcrResult] = useState<OcrTestResult | null>(null)
  const [deps, setDeps] = useState<DependencyStatus[]>([])
  const [appConfig, setAppConfig] = useState<ConfigInfo | null>(null)
  const [migrating, setMigrating] = useState(false)
  const [migrationStage, setMigrationStage] = useState<string | null>(null)
  const [migrationProgress, setMigrationProgress] = useState(0)
  const [loPath, setLoPath] = useState<string>('')
  const [localError, setLocalError] = useState<string | null>(null)
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    listOcrEngines().then(setOcrEngines).catch(() => {})
  }, [])

  useEffect(() => {
    checkDependencies().then(setDeps).catch(() => {})
  }, [])

  useEffect(() => {
    getConfig().then(setAppConfig).catch(() => {})
  }, [])

  useEffect(() => {
    const unlisteners: (() => void)[] = []
    listen<MigrationProgress>('migration-progress', e => {
      setMigrationProgress(e.payload.progress)
      setMigrationStage(e.payload.stage)
    }).then(u => unlisteners.push(u))
    listen<MigrationWarning>('migration-warning', e => {
      void message(e.payload.message, { title: '迁移警告', kind: 'warning' })
    }).then(u => unlisteners.push(u))
    return () => {
      unlisteners.forEach(u => u())
    }
  }, [])

  useEffect(() => {
    if (appConfig) {
      setLoPath(appConfig.lo_binary_path || 'soffice')
    }
  }, [appConfig])

  const handleTestOcr = async () => {
    const engineType = settings['ocr_engine'] ?? 'PaddleOCR'
    setOcrTesting(true)
    setOcrResult(null)
    try {
      const result = await testOcrEngine(engineType)
      setOcrResult(result)
    } catch (e) {
      setOcrResult({ success: false, text: '', duration_ms: 0, error: String(e) })
    } finally {
      setOcrTesting(false)
    }
  }

  const handleChangeDataDir = async () => {
    const selected = await open({ directory: true, multiple: false, title: t('data_directory') })
    if (!selected || !appConfig) return
    if (selected === appConfig.data_dir) return

    setMigrating(true)
    setMigrationProgress(0)
    setMigrationStage('preparing')
    try {
      const msg = await migrateData(appConfig.data_dir, selected)
      setAppConfig({ ...appConfig, data_dir: selected })
      await updateConfig({ data_dir: selected })
      setLocalError(null)
      setMigrating(false)
      const restart = await ask(msg, { title: '迁移完成', kind: 'info' })
      if (restart) await restartApp()
    } catch (e: unknown) {
      const err = e instanceof Error ? e.message : String(e)
      setLocalError(`迁移失败: ${err}`)
      await message(`迁移失败:\n${err}`, { title: '迁移失败', kind: 'error' })
    } finally {
      setMigrating(false)
      setMigrationStage(null)
    }
  }

  const handleChangeLang = async (newLang: string) => {
    await setLang(newLang as 'zh' | 'en')
  }

  const selectedEngine = ocrEngines.find(e => e.engine_type === (settings['ocr_engine'] ?? 'PaddleOCR'))

  const handleFieldChange = (key: string, value: string) => {
    setValue(key, value)
    setLocalError(null)
    if (saveTimerRef.current) clearTimeout(saveTimerRef.current)
    saveTimerRef.current = setTimeout(() => {
      updateSettings({ ...settings, [key]: value })
        .catch(e => setLocalError(e instanceof Error ? e.message : 'Failed to save setting'))
    }, 300)
  }

  useEffect(() => {
    return () => {
      if (saveTimerRef.current) clearTimeout(saveTimerRef.current)
    }
  }, [])

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <LoadingSpinner className="size-6 text-blue-500" />
      </div>
    )
  }

  if (error) {
    return (
      <div className="h-full p-6">
        <div className="px-4 py-3 text-sm text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-900 rounded-lg">
          {error}
        </div>
      </div>
    )
  }

  return (
    <div className="h-full p-6 overflow-y-auto">
      <div className="flex items-center justify-between mb-6">
        <div>
          <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">{t('settings')}</h2>
          <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
            Configure indexing and search preferences
          </p>
        </div>
      </div>

      {localError && (
        <div className="px-4 py-3 mb-4 text-sm text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-900 rounded-lg">
          {localError}
        </div>
      )}

      <div className="space-y-6 max-w-xl">
        <Section title={t('data_directory')}>
          <div className="text-sm text-gray-700 dark:text-gray-300 font-mono break-all">
            {appConfig?.data_dir || t('loading')}
          </div>
          <button
            onClick={handleChangeDataDir}
            disabled={migrating}
            className="mt-2 flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded-md hover:bg-blue-100 dark:hover:bg-blue-900/50 disabled:opacity-50 transition-colors"
          >
            {migrating && <LoadingSpinner className="size-3" />}
            {t('migrate_data')}
          </button>
          {migrating && (
            <div className="mt-2">
              <div className="w-full h-1.5 bg-gray-200 dark:bg-gray-700 rounded-full overflow-hidden">
                <div
                  className="h-full bg-blue-500 transition-all"
                  style={{ width: `${migrationProgress}%` }}
                />
              </div>
              <p className="text-xs text-gray-500 dark:text-gray-400 mt-1">{migrationStage}</p>
            </div>
          )}
        </Section>

        <Section title={t('libreoffice_path')}>
          <div className="flex gap-2">
            <input
              type="text"
              value={loPath}
              onChange={e => setLoPath(e.target.value)}
              onBlur={async () => {
                if (appConfig) {
                  await updateConfig({ ...appConfig, lo_binary_path: loPath })
                    .catch(err => setLocalError(err instanceof Error ? err.message : 'Failed to save LO path'))
                }
              }}
              placeholder="soffice (or full path)"
              className="flex-1 px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
            />
          </div>
          {loPath && loPath !== 'soffice' && (
            <p className="text-xs text-gray-500 dark:text-gray-400 mt-1">
              自定义路径: {loPath}
            </p>
          )}
        </Section>

        <Section title={t('ocr_engine')}>
          <div className="space-y-2">
            {ocrEngines.map(engine => (
              <label key={engine.engine_type} className="flex items-center gap-3 p-2 rounded-lg hover:bg-gray-50 dark:hover:bg-gray-800 cursor-pointer">
                <input
                  type="radio"
                  name="ocr_engine"
                  value={engine.engine_type}
                  checked={settings['ocr_engine'] === engine.engine_type}
                  onChange={() => handleFieldChange('ocr_engine', engine.engine_type)}
                  className="text-blue-600"
                />
                <div className="flex-1">
                  <span className="text-sm text-gray-900 dark:text-gray-100">{engine.name}</span>
                  {!engine.available && (
                    <span className="ml-2 text-xs text-amber-600 dark:text-amber-400">({t('not_installed')})</span>
                  )}
                </div>
              </label>
            ))}
          </div>

          <button
            onClick={handleTestOcr}
            disabled={ocrTesting}
            className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded-lg hover:bg-blue-100 dark:hover:bg-blue-900/50 disabled:opacity-50 transition-colors"
          >
            {ocrTesting && <LoadingSpinner className="size-4" />}
            {t('test_ocr')}
          </button>

          {ocrResult && (
            <div className={`p-3 rounded-lg text-sm ${
              ocrResult.success
                ? 'bg-green-50 dark:bg-green-900/20 text-green-700 dark:text-green-400'
                : 'bg-red-50 dark:bg-red-900/20 text-red-700 dark:text-red-400'
            }`}>
              {ocrResult.success
                ? `✅ ${t('ocr_test_success')}: "${ocrResult.text}" (${ocrResult.duration_ms}ms)`
                : `❌ ${t('ocr_test_failed')}: ${ocrResult.error ?? 'Unknown'}`
              }
            </div>
          )}

          {selectedEngine && !selectedEngine.available && (
            <div className="p-3 bg-amber-50 dark:bg-amber-900/20 rounded-lg text-sm text-amber-700 dark:text-amber-400">
              <p className="font-medium mb-2">Install Guide:</p>
              <pre className="whitespace-pre-wrap text-xs font-mono">{selectedEngine.install_guide}</pre>
            </div>
          )}

          <p className="text-xs text-gray-500 dark:text-gray-400">
            ⚠️ {t('ocr_engine_required')}
          </p>
        </Section>

        <Section title="OCR">
          <SelectField
            label="Language"
            value={settings['ocr_lang'] ?? 'eng'}
            onChange={v => handleFieldChange('ocr_lang', v)}
            options={OCR_LANGS}
          />
          <NumberField
            label="Concurrency"
            value={parseInt(settings['ocr_concurrency'] ?? '2', 10)}
            onChange={v => handleFieldChange('ocr_concurrency', String(v))}
            min={1}
            max={16}
            placeholder="Default: 2"
          />
        </Section>

        <Section title={t('language')}>
          <SelectField
            label={t('language')}
            value={lang}
            onChange={handleChangeLang}
            options={LANG_OPTIONS.map(o => ({ value: o.value, label: t(o.labelKey) }))}
          />
        </Section>

        <Section title="System">
          <ToggleField
            label="Launch on system startup"
            checked={settings['auto_start'] === 'true'}
            onChange={v => handleFieldChange('auto_start', v ? 'true' : 'false')}
          />
        </Section>

        <Section title="Scheduling">
          <TextField
            label="Scheduled scan time"
            value={settings['scan_time'] ?? '02:00'}
            onChange={v => handleFieldChange('scan_time', v)}
            placeholder="Default: 02:00 (2 AM)"
          />
          <ToggleField
            label="Auto backup"
            checked={settings['auto_backup'] === 'true'}
            onChange={v => handleFieldChange('auto_backup', v ? 'true' : 'false')}
          />
          <NumberField
            label="Backup interval (days)"
            value={parseInt(settings['backup_interval'] ?? '7', 10)}
            onChange={v => handleFieldChange('backup_interval', String(v))}
            min={1}
            max={365}
            placeholder="Default: 7"
          />
          <NumberField
            label="Maximum search results"
            value={parseInt(settings['max_results'] ?? '1000', 10)}
            onChange={v => handleFieldChange('max_results', String(v))}
            min={100}
            max={10000}
            step={100}
            placeholder="Default: 1000"
          />
        </Section>

        <Section title="Exclusions">
          <TextareaField
            label="Exclude patterns"
            value={settings['exclude_patterns'] ?? ''}
            onChange={v => handleFieldChange('exclude_patterns', v)}
            placeholder="*.tmp&#10;node_modules&#10;.git"
            rows={4}
          />
        </Section>

        <Section title={t('dependencies')}>
          {deps.map(dep => (
            <div key={dep.command} className="flex items-start gap-3">
              {dep.available
                ? <span className="text-green-600 dark:text-green-400 text-lg">✓</span>
                : <span className="text-amber-600 dark:text-amber-400 text-lg">✗</span>
              }
              <div className="flex-1">
                <p className="text-sm text-gray-900 dark:text-gray-100">{dep.name}</p>
                <p className="text-xs text-gray-500">{dep.command}</p>
                {!dep.available && (
                  <pre className="mt-1 text-xs text-gray-600 dark:text-gray-400 whitespace-pre-wrap bg-gray-50 dark:bg-gray-800 p-2 rounded">{filterGuide(dep.install_guide)}</pre>
                )}
              </div>
            </div>
          ))}
        </Section>

        <Section title={t('theme')}>
          <SelectField
            label={t('theme')}
            value={theme}
            onChange={v => setTheme(v as 'light' | 'dark' | 'system')}
            options={[
              { value: 'light', label: t('light') },
              { value: 'dark', label: t('dark') },
              { value: 'system', label: t('system') },
            ]}
          />
        </Section>

        <Section title="外部依赖">
          <div className="space-y-2">
            <DepRow name="PaddleOCR（内置）" status="✅" cmd="" />
            {deps.filter(d => d.command !== 'tesseract').map(dep => (
              <DepRow
                key={dep.command}
                name={dep.name}
                status={dep.available ? '✅' : '❌'}
                cmd={filterGuide(dep.install_guide)}
              />
            ))}
          </div>
        </Section>
      </div>
    </div>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg">
      <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 mb-4">{title}</h3>
      <div className="space-y-4">{children}</div>
    </div>
  )
}

function TextField({ label, value, onChange, placeholder }: {
  label: string; value: string; onChange: (v: string) => void; placeholder?: string
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{label}</label>
      <input
        type="text"
        value={value}
        onChange={e => onChange(e.target.value)}
        placeholder={placeholder}
        className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
      />
    </div>
  )
}

function SelectField({ label, value, onChange, options }: {
  label: string; value: string; onChange: (v: string) => void; options: { value: string; label: string }[]
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{label}</label>
      <select
        value={value}
        onChange={e => onChange(e.target.value)}
        className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
      >
        {options.map(o => (
          <option key={o.value} value={o.value}>{o.label}</option>
        ))}
      </select>
    </div>
  )
}

function NumberField({ label, value, onChange, min, max, step, placeholder }: {
  label: string; value: number; onChange: (v: number) => void; min: number; max: number; step?: number; placeholder?: string
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{label}</label>
      <input
        type="number"
        value={value}
        onChange={e => {
          const v = parseInt(e.target.value, 10)
          onChange(Number.isNaN(v) ? min : Math.max(min, v))
        }}
        min={min}
        max={max}
        step={step}
        placeholder={placeholder}
        className="w-24 px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
      />
    </div>
  )
}

function TextareaField({ label, value, onChange, placeholder, rows }: {
  label: string; value: string; onChange: (v: string) => void; placeholder?: string; rows?: number
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{label}</label>
      <textarea
        value={value}
        onChange={e => onChange(e.target.value)}
        placeholder={placeholder}
        rows={rows ?? 3}
        className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors resize-vertical font-mono"
      />
    </div>
  )
}

function ToggleField({ label, checked, onChange }: {
  label: string; checked: boolean; onChange: (v: boolean) => void
}) {
  return (
    <label className="flex items-center gap-3 cursor-pointer">
      <input
        type="checkbox"
        checked={checked}
        onChange={e => onChange(e.target.checked)}
        className="rounded border-gray-300 dark:border-gray-600 text-blue-600 focus:ring-blue-500 dark:bg-gray-700"
      />
      <span className="text-sm text-gray-700 dark:text-gray-300">{label}</span>
    </label>
  )
}

function filterGuide(guide: string): string {
  const platform = navigator.platform.startsWith('Mac') ? 'macOS'
    : navigator.platform.startsWith('Win') ? 'Windows'
    : 'Linux'
  const prefix = `${platform}:`
  const line = guide.split('\n').find(l => l.startsWith(prefix))
  return line ? line.slice(prefix.length).trimStart() : guide
}

function DepRow({ name, status, cmd }: { name: string; status: string; cmd: string }) {
  const [copied, setCopied] = useState(false)
  const handleCopy = async () => {
    if (!cmd) return
    try {
      await navigator.clipboard.writeText(cmd)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    } catch { /* clipboard unavailable */ }
  }
  return (
    <div className="flex items-start gap-2 py-2">
      <span className="text-base mt-0.5">{status}</span>
      <div className="flex-1 min-w-0">
        <p className="text-sm text-gray-900 dark:text-gray-100">{name}</p>
        {cmd && (
          <div className="flex items-center gap-2 mt-1">
            <pre className="flex-1 text-xs font-mono text-gray-500 dark:text-gray-400 whitespace-pre-wrap break-all">{cmd}</pre>
            <button
              onClick={handleCopy}
              className="shrink-0 px-2 py-1 text-xs text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
            >
              {copied ? '已复制' : '📋 复制'}
            </button>
          </div>
        )}
      </div>
    </div>
  )
}
