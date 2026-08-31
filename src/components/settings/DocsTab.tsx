import { useI18n } from '../../i18n'
import { Section } from './SettingsFields'

export function DocsTab() {
  const { t } = useI18n()

  return (
    <div className="space-y-6">
      <Section title={t('doc_engine')}>
        <p className="text-sm text-gray-600 dark:text-gray-400">
          <span className="font-semibold text-gray-900 dark:text-gray-100">Native</span>
          {' '}— {t('doc_engine_desc')}
        </p>
      </Section>
    </div>
  )
}
