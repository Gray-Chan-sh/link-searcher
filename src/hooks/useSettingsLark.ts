import { useCallback, useEffect, useRef, useState } from 'react'
import { listen } from '../api/client'
import { useI18n } from '../i18n'
import {
  cancelLarkPairing,
  getLarkStatus,
  larkBindCancel,
  larkBindDismiss,
  larkBindStart,
  larkBindStatus,
  larkSelfCheck,
  restartLarkBot,
  setLarkConfig,
  startLarkPairing,
  type LarkBindRequest,
  type LarkBindStatus,
  type LarkIntegration,
  type LarkSelfCheck,
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
 * 2. **订阅 `config-changed`**（`config_watch.rs` 的热重载事件）：手改 `config.json`、
 *    以及**经服务端**的写（bot 配对消费 / 远程改策略 / 本页保存）都会广播它。
 *    不订阅的话面板会一直显示旧值。
 *    ⚠️ 这条链曾经断过两处（2026-10-16 真机验收发现）：① 本进程的写被「自写抑制」
 *    挡掉、根本不发事件；② 该事件没进 `webapi::BRIDGED_EVENTS`，浏览器模式收不到。
 *    现已修复，并由 `src-tauri/src/webapi/mod.rs` 的
 *    `bridged_events_cover_frontend_subscriptions` 测试守住 ②。
 * 3. 事件仍可能丢（SSE 断线、页面长时间后台被挂起）→ 倒计时归零时**再问一次服务端**兜底。
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
  // ── 接入自检 / 绑定（`lark_setup.rs`）──
  const [check, setCheck] = useState<LarkSelfCheck | null>(null)
  const [checking, setChecking] = useState(false)
  const [checkError, setCheckError] = useState<string | null>(null)
  const [bind, setBind] = useState<LarkBindStatus | null>(null)
  const [bindStarting, setBindStarting] = useState(false)
  const [bindError, setBindError] = useState<string | null>(null)

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

  // 本地倒计时归零 → 向服务端确认一次（事件可能丢：SSE 断线、他处消费）。
  // 只在"确实倒数过"之后触发，避免首次加载时多打一次请求。
  // 不会自旋：服务端的剩余秒数随时间单调递减，最终必然回到 0 / inactive。
  const countedDown = useRef(false)
  useEffect(() => {
    if (secondsLeft > 0) {
      countedDown.current = true
      return
    }
    if (countedDown.current) {
      countedDown.current = false
      void reload()
    }
  }, [secondsLeft, reload])

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

  // ── 接入自检 / 绑定 ─────────────────────────────────────────────────────────
  // 自检要**起子进程**（`lark-cli doctor` 约 2s、含联网），所以：进面板跑一次 + 手动"重新自检"，
  // **绝不放进轮询** —— 轮询只拉绑定会话状态（纯内存读）。
  const runCheck = useCallback(async () => {
    setChecking(true)
    setCheckError(null)
    try {
      setCheck(await larkSelfCheck())
    } catch (e) {
      setCheckError(e instanceof Error ? e.message : String(e))
    } finally {
      setChecking(false)
    }
  }, [])

  useEffect(() => {
    void runCheck()
  }, [runCheck])

  const startBind = async (req: LarkBindRequest) => {
    setBindStarting(true)
    setBindError(null)
    try {
      await larkBindStart(req)
      setBind(await larkBindStatus())
    } catch (e) {
      setBindError(e instanceof Error ? e.message : String(e))
    } finally {
      setBindStarting(false)
    }
  }

  const cancelBind = async () => {
    setBindError(null)
    try {
      await larkBindCancel()
      setBind(await larkBindStatus())
    } catch (e) {
      setBindError(e instanceof Error ? e.message : String(e))
    }
  }

  const dismissBind = async () => {
    try {
      await larkBindDismiss()
    } catch {
      // 收起失败无所谓：下次进来还是会拉到同一个会话
    }
    setBind(null)
  }

  // 会话期间每秒拉一次（`running || !finalized` —— 收尾还要联网判定，得等它完）。
  useEffect(() => {
    if (!bind || (!bind.running && bind.finalized)) return
    const id = window.setTimeout(() => {
      void larkBindStatus()
        .then(setBind)
        .catch(() => {})
    }, 1000)
    return () => window.clearTimeout(id)
  }, [bind])

  // 会话收尾 → 自检重跑 + 策略状态重拉（绑定成功会重启 bot，进程态跟着变）。
  const bindDoneSeen = useRef(false)
  useEffect(() => {
    if (!bind || !bind.finalized) {
      bindDoneSeen.current = false
      return
    }
    if (bindDoneSeen.current) return
    bindDoneSeen.current = true
    void runCheck()
    void reload()
  }, [bind, runCheck, reload])

  return {
    loading, status, current, dirty, secondsLeft,
    usersText, dirsText, setUsersText, setDirsText,
    saving, busy, error, notice,
    setEnabled, setRequireP2p,
    save, beginPairing, cancelPairing, restart,
    check, checking, checkError, runCheck,
    bind, bindStarting, bindError, startBind, cancelBind, dismissBind,
  }
}
