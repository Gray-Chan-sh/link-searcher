import { useI18n } from '../../i18n'
import { LoadingSpinner } from '../../icons'
import { useSettingsLark } from '../../hooks/useSettingsLark'
import { Section, TextareaField, ToggleField, RowAction } from './SettingsFields'

/** 剩余秒数 → `mm:ss`。 */
function fmtRemaining(secs: number): string {
  const m = Math.floor(secs / 60)
  const s = secs % 60
  return `${m}:${String(s).padStart(2, '0')}`
}

const BTN_PRIMARY =
  'px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg transition-colors'

/**
 * 设置页「集成」面板 —— 飞书机器人策略 + 配对（`docs/LARK_BOT.md` §7.1）。
 *
 * 三处刻意的取舍：
 * - **写策略＝重启 bot**：策略是 bot 启动时读一次的，所以保存后服务端会重启它；
 *   面板刷新后看 `运行中` 就知道有没有起来（看配置会撒谎）。
 * - **两个名单的文本方向相反**：`allowed_users` 空＝拒绝全部；`export_allow_dirs` 空＝不限制。
 *   各自的 hint 里都写清楚了，别"顺手统一"。
 * - **配对倒计时只是显示**：到期与否由服务端裁决，UI 关掉计时就没了。
 */
export function LarkTab() {
  const { t } = useI18n()
  const lark = useSettingsLark()

  if (lark.loading) {
    return (
      <div className="flex items-center justify-center h-40">
        <LoadingSpinner className="size-6 text-blue-500" />
      </div>
    )
  }

  const cfg = lark.current ?? lark.status?.config
  if (!cfg || !lark.status) {
    return (
      <div className="space-y-6">
        <Section title={t('lark_title')}>
          <p className="text-sm text-red-600 dark:text-red-400">{lark.error ?? t('lark_load_failed')}</p>
        </Section>
      </div>
    )
  }

  const running = lark.status.running
  const pairing = cfg.pairing

  return (
    <div className="space-y-6">
      <Section title={t('lark_title')}>
        <p className="text-sm text-gray-500 dark:text-gray-400">{t('lark_desc')}</p>

        <ToggleField label={t('lark_enable')} checked={cfg.enabled} onChange={lark.setEnabled} />

        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
          <span className={running ? 'text-green-600 dark:text-green-400' : 'text-gray-400 dark:text-gray-500'}>
            {t('lark_running')}: {running ? t('lark_running_on') : t('lark_running_off')}
          </span>
          <span className="text-gray-400 dark:text-gray-500">
            {t('lark_bindings')}: {lark.status.bindings}
          </span>
        </div>

        <p className="text-xs text-gray-400 dark:text-gray-500">{t('lark_enable_hint')}</p>

        <ToggleField
          label={t('lark_require_p2p')}
          checked={cfg.require_p2p}
          onChange={lark.setRequireP2p}
        />
        <p className="text-xs text-gray-400 dark:text-gray-500">{t('lark_require_p2p_hint')}</p>

        <TextareaField
          label={t('lark_allowed_users')}
          value={lark.usersText}
          onChange={lark.setUsersText}
          rows={4}
          placeholder="ou_xxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        />
        <p className="text-xs text-gray-400 dark:text-gray-500">{t('lark_allowed_users_hint')}</p>

        <TextareaField
          label={t('lark_export_dirs')}
          value={lark.dirsText}
          onChange={lark.setDirsText}
          rows={3}
          placeholder="D:\\docs\\public"
        />
        <p className="text-xs text-gray-400 dark:text-gray-500">{t('lark_export_dirs_hint')}</p>

        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => void lark.save()}
            disabled={!lark.dirty || lark.saving || lark.busy}
            className={`flex items-center gap-1.5 ${BTN_PRIMARY}`}
          >
            {lark.saving && <LoadingSpinner className="size-3" />}
            {t('save')}
          </button>
          <RowAction onClick={() => void lark.restart()} disabled={lark.busy}>
            {t('lark_restart')}
          </RowAction>
        </div>

        {lark.notice && <p className="text-xs text-green-600 dark:text-green-400">{lark.notice}</p>}
        {lark.error && <p className="text-xs text-red-600 dark:text-red-400">{lark.error}</p>}
      </Section>

      <Section title={t('lark_pairing')}>
        <p className="text-sm text-gray-500 dark:text-gray-400">{t('lark_pairing_desc')}</p>

        {pairing && (
          <p className="text-xs text-blue-600 dark:text-blue-400">
            {lark.secondsLeft > 0
              ? t('lark_pairing_active', { time: fmtRemaining(lark.secondsLeft) })
              : t('lark_pairing_expired')}
          </p>
        )}

        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => void lark.beginPairing()}
            disabled={lark.busy || !cfg.enabled}
            className={BTN_PRIMARY}
          >
            {t('lark_pairing_start')}
          </button>
          {pairing && (
            <RowAction danger onClick={() => void lark.cancelPairing()} disabled={lark.busy}>
              {t('lark_pairing_cancel')}
            </RowAction>
          )}
        </div>

        {!cfg.enabled && (
          <p className="text-xs text-amber-600 dark:text-amber-400">{t('lark_pairing_need_enable')}</p>
        )}
      </Section>
    </div>
  )
}
