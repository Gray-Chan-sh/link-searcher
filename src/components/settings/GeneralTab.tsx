import { useI18n } from '../../i18n'
import { LoadingSpinner } from '../../icons'
import type { ConfigInfo } from '../../api/config'
import { Section, SelectField } from './SettingsFields'

const LANG_OPTIONS = [
  { value: 'zh', labelKey: 'chinese' },
  { value: 'en', labelKey: 'english' },
  { value: 'ja', labelKey: 'japanese' },
  { value: 'ko', labelKey: 'korean' },
]

interface GeneralTabProps {
  appConfig: ConfigInfo | null
  migrating: boolean
  migrationProgress: number
  migrationStage: string | null
  lang: string
  theme: string
  onChangeLang: (lang: string) => void
  onChangeTheme: (theme: string) => void
  onChangeDataDir: () => void
}

export function GeneralTab({ appConfig, migrating, migrationProgress, migrationStage, lang, theme, onChangeLang, onChangeTheme, onChangeDataDir }: GeneralTabProps) {
  const { t } = useI18n()

  return (
    <div className="space-y-6">
      <Section title={t('data_directory')}>
        <div className="text-sm text-gray-700 dark:text-gray-300 font-mono break-all">
          {appConfig?.data_dir || t('loading')}
        </div>
        <button
          onClick={onChangeDataDir}
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

      <Section title={t('language')}>
        <SelectField
          label={t('language')}
          value={lang}
          onChange={onChangeLang}
          options={LANG_OPTIONS.map(o => ({ value: o.value, label: t(o.labelKey) }))}
        />
      </Section>

      <Section title={t('theme')}>
        <SelectField
          label={t('theme')}
          value={theme}
          onChange={onChangeTheme}
          options={[
            { value: 'light', label: t('light') },
            { value: 'dark', label: t('dark') },
            { value: 'system', label: t('system') },
          ]}
        />
      </Section>
    </div>
  )
}
