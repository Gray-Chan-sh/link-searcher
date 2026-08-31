import { useI18n } from '../../i18n'
import { LoadingSpinner } from '../../icons'
import type { OcrEngineStatus, OcrTestResult, DependencyStatus } from '../../api/settings'
import { Section, SelectField, filterGuide } from './SettingsFields'

const OCR_LANGS = [
  { value: 'eng', label: 'English' },
  { value: 'chi_sim', label: 'Chinese (Simplified)' },
  { value: 'jpn', label: 'Japanese' },
  { value: 'kor', label: 'Korean' },
]

interface IndexTabProps {
  ocrEngines: OcrEngineStatus[]
  ocrTesting: boolean
  ocrResult: OcrTestResult | null
  selectedEngine: OcrEngineStatus | undefined
  ocrLang: string
  deps: DependencyStatus[]
  funasrInstalling: boolean
  onTestOcr: () => void
  onChangeOcrEngine: (engineType: string) => void
  onChangeOcrLang: (lang: string) => void
  onInstallFunasr: () => void
}

export function IndexTab({ ocrEngines, ocrTesting, ocrResult, selectedEngine, ocrLang, deps, funasrInstalling, onTestOcr, onChangeOcrEngine, onChangeOcrLang, onInstallFunasr }: IndexTabProps) {
  const { t } = useI18n()

  return (
    <div className="space-y-6">
      <Section title={t('ocr_engine')}>
        <p className="text-sm text-gray-600 dark:text-gray-400 mb-2">
          当前: <span className="font-semibold text-gray-900 dark:text-gray-100">{selectedEngine?.name ?? '未选择'}</span>
        </p>
        <div className="space-y-2">
          {ocrEngines.map(engine => (
            <label key={engine.engine_type} className="flex items-center gap-3 p-2 rounded-lg hover:bg-gray-50 dark:hover:bg-gray-800 cursor-pointer">
              <input
                type="radio"
                name="ocr_engine"
                value={engine.engine_type}
                checked={selectedEngine?.engine_type === engine.engine_type}
                onChange={() => onChangeOcrEngine(engine.engine_type)}
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
          onClick={onTestOcr}
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
            <pre className="whitespace-pre-wrap text-xs font-mono">{filterGuide(selectedEngine.install_guide)}</pre>
          </div>
        )}

        <p className="text-xs text-gray-500 dark:text-gray-400">
          ⚠️ {t('ocr_engine_required')}
        </p>
      </Section>

      <Section title={t('ocr_lang_section')}>
        <SelectField
          label={t('ocr_lang')}
          value={ocrLang}
          onChange={onChangeOcrLang}
          options={OCR_LANGS}
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
            {!dep.available && dep.name.includes('FunASR') && (
              <button
                onClick={onInstallFunasr}
                disabled={funasrInstalling}
                className="shrink-0 px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed rounded-lg transition-colors"
              >
                {funasrInstalling ? t('installing') : t('install_now')}
              </button>
            )}
          </div>
        ))}
      </Section>
    </div>
  )
}
