import { useEffect, useState } from 'react'
import { useI18n } from '../../i18n'
import { Section, TextField, NumberField, TextareaField, ToggleField, SelectField } from './SettingsFields'
import { invoke } from '../../api/client'

interface SystemTabProps {
  settings: Record<string, string>
  onFieldChange: (key: string, value: string) => void
  onRegenerateToken: () => void
}

/** 与 Rust 端 `webapi::session::status_json` 对齐（缺字段时按默认处理）。 */
interface WebSessionStatus {
  active?: boolean
  ip?: string
  last_seen?: number
  expires_in?: number
  lan_ip?: string
  port?: number
  bind?: string
}

export function SystemTab({ settings, onFieldChange, onRegenerateToken }: SystemTabProps) {
  const { t } = useI18n()
  const [session, setSession] = useState<WebSessionStatus | null>(null)
  const [copied, setCopied] = useState(false)

  // 单用户会话状态 + 局域网地址（桌面端走 Tauri 命令，Web 端走 GET /api/session）。
  const refreshSession = () => {
    invoke<WebSessionStatus>('web_session_status')
      .then(s => setSession(s))
      .catch(() => setSession(null))
  }
  useEffect(() => {
    refreshSession()
    const id = window.setInterval(refreshSession, 10000)
    return () => window.clearInterval(id)
  }, [])

  // 默认开启：只有显式存了 "false" 才算关闭（与 lib.rs 启动判定一致）。
  const webEnabled = settings['web_api_enabled'] !== 'false'
  const port = String(session?.port ?? settings['web_api_port'] ?? '8443')
  const lanHost = session?.lan_ip && session.lan_ip !== '127.0.0.1' ? session.lan_ip : '127.0.0.1'
  const localUrl = `https://127.0.0.1:${port}`
  const remoteUrl = `https://${lanHost}:${port}`

  const copyRemoteUrl = async () => {
    try {
      await navigator.clipboard.writeText(remoteUrl)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 1500)
    } catch { /* 剪贴板不可用（非安全上下文）时忽略 */ }
  }

  const forceLogout = async () => {
    try {
      await invoke('web_session_logout')
      refreshSession()
    } catch { /* 无会话可踢 */ }
  }

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
          label="启用 Web API（默认开启，启动即监听）"
          checked={webEnabled}
          onChange={v => onFieldChange('web_api_enabled', v ? 'true' : 'false')}
        />
        {webEnabled && (
          <>
            <div className="p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-900 rounded-lg text-sm text-amber-700 dark:text-amber-400">
              ⚠️ 修改开关需要重启应用后才会生效（端口 / 绑定地址同理）
            </div>
            <ToggleField
              label="开发模式（代理到 Vite dev server，需先运行 npm run dev）"
              checked={settings['web_api_dev_mode'] === 'true'}
              onChange={v => onFieldChange('web_api_dev_mode', v ? 'true' : 'false')}
            />
            <div className="text-sm text-gray-700 dark:text-gray-300 space-y-1">
              <div>
                <span className="text-gray-500 dark:text-gray-400">本机访问：</span>
                <span className="font-mono">{localUrl}</span>
              </div>
              <div className="flex items-center gap-2 flex-wrap">
                <span className="text-gray-500 dark:text-gray-400">远程访问：</span>
                <span className="font-mono">{remoteUrl}</span>
                <button
                  type="button"
                  onClick={copyRemoteUrl}
                  className="px-2 py-0.5 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
                >
                  {copied ? '已复制' : '复制'}
                </button>
              </div>
              <div className="text-xs text-gray-500 dark:text-gray-400">
                其它机器用浏览器打开远程地址并输入下方 Token 即可使用（自签名证书需在浏览器中信任一次）。
              </div>
            </div>
            {/* 单用户会话：谁连着 / 强制踢出 */}
            <div className="p-3 bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg text-sm flex items-center justify-between gap-3 flex-wrap">
              <span className="text-gray-700 dark:text-gray-300">
                {session?.active && session.ip
                  ? <>当前在线：<span className="font-mono">{session.ip}</span>
                      {typeof session.last_seen === 'number' &&
                        <> · 最后活跃 {new Date(session.last_seen * 1000).toLocaleTimeString()}</>}
                    </>
                  : '当前无 Web 连接（单用户模式，同一时刻仅允许一个会话）'}
              </span>
              {session?.active && (
                <button
                  type="button"
                  onClick={forceLogout}
                  className="shrink-0 px-3 py-1 text-xs font-medium text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/30 rounded-lg hover:bg-red-100 dark:hover:bg-red-900/50 transition-colors"
                >
                  强制退出
                </button>
              )}
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
        <NumberField
          label="会话空闲超时（秒）：超过后自动释放，其它设备方可登录"
          value={parseInt(settings['web_session_timeout_secs'] ?? '600', 10)}
          onChange={v => onFieldChange('web_session_timeout_secs', String(v))}
          min={60}
          max={86400}
          placeholder="默认: 600"
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
