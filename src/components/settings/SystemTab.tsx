import { useI18n } from '../../i18n'
import { Section, TextField, NumberField, TextareaField, ToggleField, SelectField } from './SettingsFields'

interface SystemTabProps {
  settings: Record<string, string>
  onFieldChange: (key: string, value: string) => void
  onRegenerateToken: () => void
}

export function SystemTab({ settings, onFieldChange, onRegenerateToken }: SystemTabProps) {
  const { t } = useI18n()

  return (
    <div className="space-y-6">
      <Section title={t('tab_system')}>
        <ToggleField
          label={t('sys_launch_on_startup')}
          checked={settings['auto_start'] === 'true'}
          onChange={v => onFieldChange('auto_start', v ? 'true' : 'false')}
        />
      </Section>

      <Section title={t('sys_scheduling')}>
        <TextField
          label={t('sys_scheduled_scan_time')}
          value={settings['scan_time'] ?? '02:00'}
          onChange={v => onFieldChange('scan_time', v)}
          placeholder="Default: 02:00 (2 AM)"
        />
        <ToggleField
          label={t('sys_auto_backup')}
          checked={settings['auto_backup'] === 'true'}
          onChange={v => onFieldChange('auto_backup', v ? 'true' : 'false')}
        />
        <NumberField
          label={t('sys_backup_interval')}
          value={parseInt(settings['backup_interval'] ?? '7', 10)}
          onChange={v => onFieldChange('backup_interval', String(v))}
          min={1}
          max={365}
          placeholder="Default: 7"
        />
        <NumberField
          label={t('sys_max_results')}
          value={parseInt(settings['max_results'] ?? '1000', 10)}
          onChange={v => onFieldChange('max_results', String(v))}
          min={100}
          max={10000}
          step={100}
          placeholder="Default: 1000"
        />
      </Section>

      <Section title={t('sys_exclusions')}>
        <TextareaField
          label={t('sys_exclude_patterns')}
          value={settings['exclude_patterns'] ?? ''}
          onChange={v => onFieldChange('exclude_patterns', v)}
          placeholder="*.tmp&#10;node_modules&#10;.git"
          rows={4}
        />
      </Section>

      <Section title="Web API">
        <ToggleField
          label="启用 Web API"
          checked={settings['web_api_enabled'] === 'true'}
          onChange={v => onFieldChange('web_api_enabled', v ? 'true' : 'false')}
        />
        {settings['web_api_enabled'] === 'true' && (
          <>
            <div className="p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-900 rounded-lg text-sm text-amber-700 dark:text-amber-400">
              ⚠️ 需要重启应用后才会启动 Web API 服务器
            </div>
            <ToggleField
              label="开发模式（代理到 Vite dev server，需先运行 npm run dev）"
              checked={settings['web_api_dev_mode'] === 'true'}
              onChange={v => onFieldChange('web_api_dev_mode', v ? 'true' : 'false')}
            />
            <div className="text-sm text-gray-700 dark:text-gray-300">
              <span className="text-gray-500 dark:text-gray-400">访问地址：</span>
              <span className="font-mono">https://127.0.0.1:{settings['web_api_port'] ?? '8443'}</span>
            </div>
          </>
        )}
        <NumberField
          label="端口"
          value={parseInt(settings['web_api_port'] ?? '8443', 10)}
          onChange={v => onFieldChange('web_api_port', String(v))}
          min={1}
          max={65535}
          placeholder="默认: 8443"
        />
        <div>
          <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Bearer Token</label>
          <div className="flex gap-2">
            <input
              type="text"
              readOnly
              value={settings['web_api_token'] ?? ''}
              className="flex-1 px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 font-mono focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
            />
            <button
              type="button"
              onClick={onRegenerateToken}
              className="shrink-0 px-3 py-2 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded-lg hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
            >
              重新生成
            </button>
          </div>
        </div>
        <SelectField
          label="绑定地址"
          value={settings['web_api_bind'] ?? '0.0.0.0'}
          onChange={v => onFieldChange('web_api_bind', v)}
          options={[
            { value: '0.0.0.0', label: '局域网 (0.0.0.0)' },
            { value: '127.0.0.1', label: '仅本机 (127.0.0.1)' },
          ]}
        />
      </Section>
    </div>
  )
}
