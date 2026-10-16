import { useState } from 'react'
import { useI18n } from '../../i18n'
import { LoadingSpinner } from '../../icons'
import { useSettingsLark } from '../../hooks/useSettingsLark'
import { Section, SelectField, TextareaField, TextField, ToggleField, RowAction } from './SettingsFields'

/** 剩余秒数 → `mm:ss`。 */
function fmtRemaining(secs: number): string {
  const m = Math.floor(secs / 60)
  const s = secs % 60
  return `${m}:${String(s).padStart(2, '0')}`
}

const BTN_PRIMARY =
  'px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg transition-colors'

/** lark-cli 原话的 `pass` / `warn` / `fail`（＋我们自己的 `ready` / `blocked`）→ 颜色。 */
function statusColor(status: string): string {
  if (status === 'pass' || status === 'ready') return 'text-green-600 dark:text-green-400'
  if (status === 'fail' || status === 'blocked') return 'text-red-600 dark:text-red-400'
  return 'text-amber-600 dark:text-amber-400'
}

/** 「下一步」的键 → i18n 键（后端只给**键**，文案与翻译留在前端）。 */
const STEP_KEYS: Record<string, string> = {
  install_cli: 'lark_step_install_cli',
  bind_app: 'lark_step_bind_app',
  rebind_app: 'lark_step_rebind_app',
  card_callback: 'lark_step_card_callback',
}

/**
 * 设置页「集成」面板 —— 飞书机器人策略 + 配对（`docs/LARK_BOT.md` §7.1）+ **接入自检 / 绑定**。
 *
 * 四处刻意的取舍：
 * - **写策略＝重启 bot**：策略是 bot 启动时读一次的，所以保存后服务端会重启它；
 *   面板刷新后看 `运行中` 就知道有没有起来（看配置会撒谎）。
 * - **两个名单的文本方向相反**：`allowed_users` 空＝拒绝全部；`export_allow_dirs` 空＝不限制。
 *   各自的 hint 里都写清楚了，别"顺手统一"。
 * - **配对倒计时只是显示**：到期与否由服务端裁决，UI 关掉计时就没了。
 * - **凭证不在本应用里**：App ID / Secret 归 `lark-cli` 管（secret 在 OS 凭据库），
 *   所以这里只做"**看得见**（自检）"与"**代跑一条命令**（绑定）"，绝不自己存密钥。
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
      <SelfCheckPanel lark={lark} />

      <BindPanel lark={lark} alreadyBound={!!lark.check?.app_id} />

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

/** 一行「名称: 值」，自检面板反复用。 */
function CheckRow({ label, value, status }: { label: string; value: string; status?: string }) {
  return (
    <>
      <dt className="text-gray-500 dark:text-gray-400">{label}</dt>
      <dd className={status ? statusColor(status) : 'text-gray-700 dark:text-gray-300'}>
        {value}
        {status && <span className="ml-1 text-gray-400 dark:text-gray-500">({status})</span>}
      </dd>
    </>
  )
}

/**
 * **接入自检**：把"前提"摊开（App ID / 两种身份 / 卡片订阅 / 下一条该做的事）。
 *
 * 为什么需要它：依赖中心的"就绪"只探可执行文件在不在，"装了但没绑定"照样 ✅；
 * 集成页以前只有「进程：运行中/未运行」+「详见日志」—— 用户不知道**缺哪一步**。
 */
function SelfCheckPanel({ lark }: { lark: ReturnType<typeof useSettingsLark> }) {
  const { t } = useI18n()
  const c = lark.check

  return (
    <Section title={t('lark_selfcheck_title')}>
      <p className="text-sm text-gray-500 dark:text-gray-400">{t('lark_selfcheck_desc')}</p>

      {lark.checking && (
        <p className="flex items-center gap-1.5 text-xs text-gray-400 dark:text-gray-500">
          <LoadingSpinner className="size-3" />
          {t('lark_selfcheck_running')}
        </p>
      )}

      {lark.checkError && (
        <p className="text-xs text-red-600 dark:text-red-400">
          {t('lark_selfcheck_failed')}：{lark.checkError}
        </p>
      )}

      {c && (
        <>
          <dl className="grid grid-cols-[7rem_1fr] gap-x-3 gap-y-1 text-xs">
            <CheckRow
              label="lark-cli"
              value={
                c.cli_path
                  ? `${c.cli_path}${c.cli_version ? `  v${c.cli_version}` : ''}`
                  : t('lark_not_found')
              }
            />
            <CheckRow
              label={t('lark_app_id')}
              value={c.app_id ? `${c.app_id}${c.brand ? ` (${c.brand})` : ''}` : t('lark_not_bound')}
            />
            <CheckRow
              label={t('lark_bot_identity')}
              value={c.bot_message || '—'}
              status={c.bot_status || undefined}
            />
            <CheckRow
              label={t('lark_user_identity')}
              value={c.user_message || '—'}
              status={c.user_status || undefined}
            />
            <CheckRow
              label={t('lark_card_callback')}
              value={c.card_callback_detail || c.card_callback}
              status={c.card_callback}
            />
          </dl>

          <p className="text-xs text-gray-400 dark:text-gray-500">{t('lark_user_identity_hint')}</p>

          {c.missing.length > 0 && (
            <p className="text-xs text-amber-600 dark:text-amber-400">
              {t('lark_next_step')}
              {c.missing.map(k => ` ${t(STEP_KEYS[k] ?? k)}`).join('')}
            </p>
          )}

          {c.error && <p className="text-xs text-red-600 dark:text-red-400">{c.error}</p>}

          {c.checks.length > 0 && (
            <details className="text-xs">
              <summary className="cursor-pointer text-gray-500 dark:text-gray-400">
                {t('lark_selfcheck_checks')}（{c.checks.length}）
              </summary>
              <ul className="mt-2 space-y-0.5">
                {c.checks.map(ch => (
                  <li key={ch.name} className={statusColor(ch.status)}>
                    <span className="font-mono">{ch.name}</span>({ch.status}): {ch.message}
                  </li>
                ))}
              </ul>
            </details>
          )}
        </>
      )}

      <div className="flex items-center gap-2">
        <RowAction onClick={() => void lark.runCheck()} disabled={lark.checking}>
          {t('lark_selfcheck_run')}
        </RowAction>
        <span className="text-xs text-gray-400 dark:text-gray-500">{t('lark_selfcheck_cost')}</span>
      </div>
    </Section>
  )
}

/**
 * **绑定飞书应用**：代跑 `lark-cli config init`（两种模式），把输出如实回显。
 *
 * 三条边界（见 `src-tauri/src/lark_setup.rs` 的文件头）：
 * - App Secret 只经 stdin 交给 lark-cli（本应用不落盘、不打日志）→ 起步后**立刻清掉本地输入**；
 * - `config init` 是**先写盘再校验**的，填错会冲掉原本能用的绑定 → 已绑定时显式警告；
 * - `--new` 会**新建一个飞书应用**（浏览器流程），不是"登录一下"。
 */
function BindPanel({
  lark,
  alreadyBound,
}: {
  lark: ReturnType<typeof useSettingsLark>
  alreadyBound: boolean
}) {
  const { t } = useI18n()
  const [mode, setMode] = useState<'new' | 'existing'>('new')
  const [appId, setAppId] = useState('')
  const [secret, setSecret] = useState('')
  const [brand, setBrand] = useState('feishu')

  const b = lark.bind
  const session = !!b && b.mode !== ''

  const start = () => {
    if (mode === 'new') {
      void lark.startBind({ mode: 'new' })
      return
    }
    const req = { mode: 'existing' as const, app_id: appId.trim(), app_secret: secret, brand }
    setSecret('') // 已经交给 lark-cli，本地不再留
    void lark.startBind(req)
  }

  return (
    <Section title={t('lark_bind_title')}>
      <p className="text-sm text-gray-500 dark:text-gray-400">{t('lark_bind_desc')}</p>

      {!session && (
        <>
          <SelectField
            label={t('lark_bind_mode')}
            value={mode}
            onChange={v => setMode(v === 'existing' ? 'existing' : 'new')}
            options={[
              { value: 'new', label: t('lark_bind_mode_new') },
              { value: 'existing', label: t('lark_bind_mode_existing') },
            ]}
          />

          {mode === 'new' ? (
            <p className="text-xs text-gray-400 dark:text-gray-500">{t('lark_bind_new_hint')}</p>
          ) : (
            <>
              <TextField
                label={t('lark_bind_app_id')}
                value={appId}
                onChange={setAppId}
                placeholder="cli_xxxxxxxxxxxxxxxx"
              />
              <div>
                <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">
                  {t('lark_bind_app_secret')}
                </label>
                {/* 用 password 输入（`SettingsFields` 的 TextField 固定 type=text，这里不为了
                    一个字段去改公共组件）：密钥不该以明文停在屏幕上。 */}
                <input
                  type="password"
                  value={secret}
                  onChange={e => setSecret(e.target.value)}
                  autoComplete="off"
                  className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
                />
              </div>
              <SelectField
                label={t('lark_bind_brand')}
                value={brand}
                onChange={setBrand}
                options={[
                  { value: 'feishu', label: 'Feishu（飞书 · 国内）' },
                  { value: 'lark', label: 'Lark（国际版）' },
                ]}
              />
              <p className="text-xs text-gray-400 dark:text-gray-500">{t('lark_bind_secret_hint')}</p>
            </>
          )}

          {alreadyBound && (
            <p className="text-xs text-amber-600 dark:text-amber-400">{t('lark_bind_overwrite_warning')}</p>
          )}

          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={start}
              disabled={lark.bindStarting || (mode === 'existing' && (!appId.trim() || !secret))}
              className={`flex items-center gap-1.5 ${BTN_PRIMARY}`}
            >
              {lark.bindStarting && <LoadingSpinner className="size-3" />}
              {mode === 'new' ? t('lark_bind_start_new') : t('lark_bind_start_existing')}
            </button>
          </div>

          {lark.bindError && <p className="text-xs text-red-600 dark:text-red-400">{lark.bindError}</p>}
        </>
      )}

      {b && (
        <>
          <p className="text-xs text-gray-500 dark:text-gray-400">
            {b.mode === 'new' ? t('lark_bind_mode_new') : t('lark_bind_mode_existing')}
            {' · '}
            {b.running
              ? t('lark_bind_running')
              : b.finalized
                ? b.success
                  ? t('lark_bind_success')
                  : t('lark_bind_failed')
                : t('lark_bind_finalizing')}
            {!b.running && b.finalized && b.exit_code !== null && ` (exit ${b.exit_code})`}
          </p>

          {b.finalized && b.success && b.app_id_after && (
            <p className="text-xs text-green-600 dark:text-green-400">
              {t('lark_app_id')}: <span className="font-mono">{b.app_id_after}</span>
            </p>
          )}

          {b.urls.map(u => (
            <div key={u} className="flex flex-wrap items-center gap-2 text-xs">
              <a
                href={u}
                target="_blank"
                rel="noopener noreferrer"
                className="text-blue-600 dark:text-blue-400 underline break-all"
              >
                {t('lark_bind_open_url')}
              </a>
              <RowAction
                onClick={() => {
                  navigator.clipboard.writeText(u).catch(() => {})
                }}
              >
                {t('lark_bind_copy_url')}
              </RowAction>
            </div>
          ))}

          {b.finalized && !b.success && (
            <p className="text-xs text-red-600 dark:text-red-400">{t('lark_bind_rollback_hint')}</p>
          )}

          {b.finalized && !b.success && b.needs_force_init && (
            <RowAction onClick={() => void lark.startBind({ mode: 'new', force_init: true })}>
              {t('lark_bind_force_init')}
            </RowAction>
          )}

          {b.error && <p className="text-xs text-red-600 dark:text-red-400">{b.error}</p>}

          {b.log.length > 0 && (
            <>
              <p className="text-xs text-gray-500 dark:text-gray-400">{t('lark_bind_log')}</p>
              <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-gray-50 p-2 text-[11px] leading-relaxed text-gray-700 dark:bg-gray-800 dark:text-gray-300">
                {b.log.join('\n')}
              </pre>
            </>
          )}

          <div className="flex items-center gap-2">
            {b.running && (
              <RowAction danger onClick={() => void lark.cancelBind()}>
                {t('lark_bind_cancel')}
              </RowAction>
            )}
            {b.finalized && (
              <RowAction onClick={() => void lark.dismissBind()}>{t('lark_bind_close')}</RowAction>
            )}
          </div>
        </>
      )}
    </Section>
  )
}
