import { useCallback, useEffect, useState } from 'react'
import { listen } from '../api/client'
import { useI18n } from '../i18n'
import {
  cancelLarkPairing,
  getLarkStatus,
  restartLarkBot,
  setLarkConfig,
  startLarkPairing,
  type LarkIntegration,
  type LarkStatus,
} from '../api/lark'

/** 配对默认时长（与 Rust `commands::lark::DEFAULT_PAIRING_MINUTES` 一致）。 */
export const DEFAULT_PAIRING_MINUTES = 5

/** 多行文本 → 列表：去空白、丢空行（服务端还会再规整一次：去重 + 去空白）。 */
export function parseList(text: string): string[] {
  return text
    .split('\n')
    .map(s => s.trim())
    .filter(Boolean)
}

/** 列表 → 多行文本（编辑用）。 */
export function joinList(list: string[]): string {
  return list.join('\n')
}

/**
 * 设置页「集成」面板的状态与动作。
 *
 * 两处**不是**随手写的地方：
 * 1. **配对倒计时只是显示**：真正的到期判定在服务端（`pairing_active`）—— UI 一关，
 *    计时就没了，所以绝不能靠前端计时来决定"还在不在配对"。
 * 2. **订阅 `config-changed`**（C3 的热重载事件）：手改 `config.json`、或 bot 配对消费
 *    成功，都会改这份配置；不订阅的话面板会一直显示旧值。
 */
export function useSettingsLark() {
  const { t } = useI18n()
  const [status, setStatus] = useState<LarkStatus | null>(null)
  const [loading, setLoading] = useState(true)
  const [draft, setDraft] = useState<LarkIntegration | null>(null)
  const [usersText, setUsersText] = useState('')
  const [dirsText, setDirsText] = useState('')
  const [saving, setSaving] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [secondsLeft, setSecondsLeft] = useState(0)

  const reload = useCallback(async () => {
    try {
      const st = await getLarkStatus()
      setStatus(st)
      setDraft(st.config)
      setUsersText(joinList(st.config.allowed_users))
      setDirsText(joinList(st.config.export_allow_dirs))
      setSecondsLeft(st.pairing_active ? st.pairing_seconds_left : 0)
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void reload()
  }, [reload])

  // 手改 config.json（热重载）/ 配对消费成功后服务端会广播；不刷新就会显示旧值。
  useEffect(() => {
    const un = listen('config-changed', () => {
      void reload()
    })
    return () => {
      un.then(f => f())
    }
  }, [reload])

  // 倒计时（仅显示）：只在配对有效时走秒，到 0 就停。
  useEffect(() => {
    if (secondsLeft <= 0) return
    const id = window.setTimeout(() => setSecondsLeft(s => (s > 0 ? s - 1 : 0)), 1000)
    return () => window.clearTimeout(id)
  }, [secondsLeft])

  /** 面板上此刻"将要保存"的值（bool 来自 draft，两个列表来自文本框）。 */
  const current: LarkIntegration | null =
    draft && { ...draft, allowed_users: parseList(usersText), export_allow_dirs: parseList(dirsText) }

  const dirty = !!current && !!status && JSON.stringify(current) !== JSON.stringify(status.config)

  const setEnabled = (v: boolean) => setDraft(d => (d ? { ...d, enabled: v } : d))
  const setRequireP2p = (v: boolean) => setDraft(d => (d ? { ...d, require_p2p: v } : d))

  const run = async (label: string, fn: () => Promise<void>) => {
    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      await fn()
      await reload()
    } catch (e) {
      setError(`${label}：${e instanceof Error ? e.message : String(e)}`)
    } finally {
      setBusy(false)
    }
  }

  const save = async () => {
    if (!current) return
    setSaving(true)
    setError(null)
    setNotice(null)
    try {
      await setLarkConfig(current)
      setNotice(t('lark_saved'))
      await reload()
    } catch (e) {
      setError(`${t('lark_save_failed')}：${e instanceof Error ? e.message : String(e)}`)
    } finally {
      setSaving(false)
    }
  }

  const beginPairing = () =>
    run(t('lark_pairing_start'), () => startLarkPairing(DEFAULT_PAIRING_MINUTES))
  const cancelPairing = () => run(t('lark_pairing_cancel'), () => cancelLarkPairing())
  const restart = () => run(t('lark_restart'), () => restartLarkBot())

  return {
    loading, status, current, dirty, secondsLeft,
    usersText, dirsText, setUsersText, setDirsText,
    saving, busy, error, notice,
    setEnabled, setRequireP2p,
    save, beginPairing, cancelPairing, restart,
  }
}
