import { useI18n } from '../../i18n'
import { LoadingSpinner } from '../../icons'
import type { OcrEngineStatus, OcrTestResult } from '../../api/settings'
import { Section, SelectField, NumberField, ToggleField, filterGuide } from './SettingsFields'

const OCR_LANGS = [
  { value: 'eng', label: 'English' },
  { value: 'chi_sim', label: 'Chinese (Simplified)' },
  { value: 'jpn', label: 'Japanese' },
  { value: 'kor', label: 'Korean' },
]

const DPI_OPTIONS = [
  { value: '200', label: '200' },
  { value: '300', label: '300 (Recommended)' },
  { value: '400', label: '400' },
  { value: '600', label: '600' },
]

interface IndexTabProps {
  ocrEngines: OcrEngineStatus[]
  ocrTesting: boolean
  ocrResult: OcrTestResult | null
  selectedEngine: OcrEngineStatus | undefined
  ocrLang: string
  ocrPdfDpi: string
  ocrStallTimeout: string
  /** 自适应并发（内存压力降级/升级）的全部设置项。 */
  loadSettings: Record<string, string>
  onTestOcr: () => void
  onChangeOcrEngine: (engineType: string) => void
  onChangeOcrLang: (lang: string) => void
  onChangeOcrPdfDpi: (dpi: string) => void
  onChangeOcrStallTimeout: (seconds: string) => void
  onChangeLoadSetting: (key: string, value: string) => void
}

export function IndexTab({ ocrEngines, ocrTesting, ocrResult, selectedEngine, ocrLang, ocrPdfDpi, ocrStallTimeout, loadSettings, onTestOcr, onChangeOcrEngine, onChangeOcrLang, onChangeOcrPdfDpi, onChangeOcrStallTimeout, onChangeLoadSetting }: IndexTabProps) {
  const { t } = useI18n()
  const loadNum = (key: string, dflt: number) => Number.parseInt(loadSettings[key] ?? String(dflt), 10) || dflt
  const setLoad = (key: string) => (v: number) => onChangeLoadSetting(key, String(v))

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

      <Section title={t('ocr_pdf_dpi')}>
        <SelectField
          label={t('ocr_pdf_dpi')}
          value={ocrPdfDpi}
          onChange={onChangeOcrPdfDpi}
          options={DPI_OPTIONS}
        />
        <p className="text-xs text-gray-500 dark:text-gray-400">
          {t('ocr_pdf_dpi_hint')}
        </p>
      </Section>

      <Section title={t('ocr_stall_timeout')}>
        <NumberField
          label={t('ocr_stall_timeout')}
          value={Number.parseInt(ocrStallTimeout, 10) || 0}
          onChange={v => onChangeOcrStallTimeout(String(v))}
          min={0}
          max={3600}
          step={30}
        />
        <p className="text-xs text-gray-500 dark:text-gray-400">
          {t('ocr_stall_timeout_hint')}
        </p>
      </Section>

      <Section title={t('adaptive_load')}>
        <ToggleField
          label={t('adaptive_load')}
          checked={loadSettings['adaptive_load_enabled'] !== '0'}
          onChange={v => onChangeLoadSetting('adaptive_load_enabled', v ? '1' : '0')}
        />
        <p className="text-xs text-gray-500 dark:text-gray-400">
          {t('adaptive_load_hint')}
        </p>
        <div className="mt-3 space-y-3">
          <NumberField
            label={t('load_process_soft_pct')}
            value={loadNum('load_process_soft_pct', 40)}
            onChange={setLoad('load_process_soft_pct')}
            min={5}
            max={95}
            step={5}
          />
          <NumberField
            label={t('load_process_hard_pct')}
            value={loadNum('load_process_hard_pct', 55)}
            onChange={setLoad('load_process_hard_pct')}
            min={5}
            max={95}
            step={5}
          />
          <NumberField
            label={t('load_process_pause_pct')}
            value={loadNum('load_process_pause_pct', 70)}
            onChange={setLoad('load_process_pause_pct')}
            min={5}
            max={95}
            step={5}
          />
          <NumberField
            label={t('load_system_soft_pct')}
            value={loadNum('load_system_soft_pct', 85)}
            onChange={setLoad('load_system_soft_pct')}
            min={10}
            max={99}
            step={1}
          />
          <NumberField
            label={t('load_system_hard_pct')}
            value={loadNum('load_system_hard_pct', 90)}
            onChange={setLoad('load_system_hard_pct')}
            min={10}
            max={99}
            step={1}
          />
          <NumberField
            label={t('load_system_pause_pct')}
            value={loadNum('load_system_pause_pct', 95)}
            onChange={setLoad('load_system_pause_pct')}
            min={10}
            max={99}
            step={1}
          />
        </div>
      </Section>
    </div>
  )
}
