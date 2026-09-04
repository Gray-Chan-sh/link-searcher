import { useCallback, useEffect, useRef, useState } from 'react'
import { listen } from '../api/client'
import {
  getSetupStatus,
  installDep,
  cancelDepInstall,
  type SetupStatus,
  type DepInstallResult,
} from '../api/settings'
import { isTauri } from '../utils/platform'

export interface ProgressState {
  dep: string
  current: number
  total: number
  bytes: number
}

/**
 * Central state for the dependency center + first-run wizard.
 * - `status`: latest snapshot from `get_setup_status`.
 * - `install`: start installing a dep id (backend is single-flight).
 * - `activeDep`: which dep is installing right now.
 * - `progress`: latest `dep-progress` payload (for a progress bar).
 * - `refresh`: re-fetch status (call after install completes / on mount).
 */
export function useSetup() {
  const [status, setStatus] = useState<SetupStatus | null>(null)
  const [loading, setLoading] = useState(true)
  const [activeDep, setActiveDep] = useState<string | null>(null)
  const [progress, setProgress] = useState<ProgressState | null>(null)
  const [lastResult, setLastResult] = useState<DepInstallResult | null>(null)
  const installingRef = useRef(false)

  const refresh = useCallback(async () => {
    if (!isTauri()) {
      setStatus({
        deps: [],
        all_recommended_ready: true,
        data_dir: '',
      })
      setLoading(false)
      return
    }
    try {
      const s = await getSetupStatus()
      setStatus(s)
      // If nothing is installing, clear stale active/progress.
      if (!installingRef.current) {
        setActiveDep(null)
        setProgress(null)
      }
    } catch (e) {
      console.error('getSetupStatus failed:', e)
    } finally {
      setLoading(false)
    }
  }, [])

  // Poll dep_install_status so the UI recovers after reloads mid-install.
  const pollActive = useCallback(async () => {
    if (!isTauri()) return
    try {
      const st = await import('../api/settings').then(m => m.depInstallStatus())
      if (st.installing) {
        installingRef.current = true
        setActiveDep(st.dep)
      }
    } catch {
      /* ignore */
    }
  }, [])

  useEffect(() => {
    void refresh()
    void pollActive()
  }, [refresh, pollActive])

  // Event listeners: progress + completion.
  useEffect(() => {
    const unlisteners: (() => void)[] = []
    listen<ProgressState>('dep-progress', p => {
      setProgress(p)
      setActiveDep(p.dep)
    }).then(u => unlisteners.push(u))
    listen<DepInstallResult>('dep-install-done', async result => {
      installingRef.current = false
      setActiveDep(null)
      setProgress(null)
      setLastResult(result)
      await refresh()
    }).then(u => unlisteners.push(u))
    return () => {
      unlisteners.forEach(u => u())
    }
  }, [refresh])

  const startInstall = useCallback(async (dep: string): Promise<DepInstallResult | null> => {
    if (installingRef.current) return null
    installingRef.current = true
    setActiveDep(dep)
    setLastResult(null)
    setProgress(null)
    try {
      await installDep(dep)
      // Completion arrives via `dep-install-done`.
      return null
    } catch (e) {
      installingRef.current = false
      setActiveDep(null)
      const msg = e instanceof Error ? e.message : String(e)
      const result: DepInstallResult = { dep, success: false, message: msg }
      setLastResult(result)
      await refresh()
      return result
    }
  }, [refresh])

  const cancelInstall = useCallback(async () => {
    try {
      await cancelDepInstall()
    } catch {
      /* no-op */
    }
  }, [])

  const isInstalling = (dep: string) => activeDep === dep && !!status

  return {
    status,
    loading,
    activeDep,
    progress,
    lastResult,
    startInstall,
    cancelInstall,
    refresh,
    isInstalling,
  }
}
