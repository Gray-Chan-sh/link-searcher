import { useCallback, useEffect, useRef, useState } from 'react'
import { getIndexStatus, triggerScan, rebuildIndex, type IndexStatus } from '../api/index'

interface UseIndexStatusReturn {
  status: IndexStatus | null
  loading: boolean
  error: string | null
  scan: (dirId?: string) => Promise<void>
  rebuild: () => Promise<void>
  refresh: () => Promise<void>
}

/** How long a single status IPC may take before we treat it as failed.
 *
 * 2026-10-15 实测：索引高峰期界面的 IPC 会被提取流水线抢连接池饿死——调用
 * `get_index_status` 后 promise 长时间不 resolve，界面既不发新请求也无法更新
 * （窗口被 Windows 标成"无响应"，WebView 还在空转烧 CPU）。给它一个硬上限，
 * 让界面"快速失败并可重试"，而不是无限等待。 */
const IPC_TIMEOUT_MS = 8_000

/** Stop polling after this many consecutive failures (avoids a retry spin). */
const MAX_CONSECUTIVE_FAILURES = 5

function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`IPC timeout after ${ms}ms`)), ms)
    p.then(
      v => { clearTimeout(t); resolve(v) },
      e => { clearTimeout(t); reject(e) },
    )
  })
}

export function useIndexStatus(): UseIndexStatusReturn {
  const [status, setStatus] = useState<IndexStatus | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  // 上一次请求还没回来就不再发新的：并发请求会在后端排队，正是"界面卡住"的放大器。
  const inFlight = useRef(false)
  const failures = useRef(0)
  const [giveUp, setGiveUp] = useState(false)

  const refresh = useCallback(async () => {
    if (inFlight.current || giveUp) return
    inFlight.current = true
    try {
      const s = await withTimeout(getIndexStatus(), IPC_TIMEOUT_MS)
      failures.current = 0
      setStatus(s)
      setError(null)
    } catch (e) {
      failures.current += 1
      setError(e instanceof Error ? e.message : 'Failed to get index status')
      if (failures.current >= MAX_CONSECUTIVE_FAILURES) {
        // 连续失败就停下来，别继续每 5 秒撞一次（否则界面会一直空转）。
        setGiveUp(true)
      }
    } finally {
      inFlight.current = false
      setLoading(false)
    }
  }, [giveUp])

  const scan = useCallback(async (dirId?: string) => {
    try {
      await triggerScan(dirId)
      failures.current = 0
      setGiveUp(false)
      void refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to trigger scan')
    }
  }, [refresh])

  const rebuild = useCallback(async () => {
    try {
      setStatus(prev => prev ? { ...prev, indexed: 0, pending: 0, errors: 0, is_scanning: true } : null)
      await rebuildIndex()
      void refresh()
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to rebuild index'
      setError(msg)
      const { alert } = await import('../utils/platform')
      await alert(msg)
    }
  }, [refresh])

  // Adaptive polling: 5s while scanning, 30s when idle. Stops entirely once the
  // status endpoint keeps failing, and resumes on the next manual action.
  useEffect(() => {
    if (giveUp) return
    void refresh()
    const interval = setInterval(refresh, (status?.is_scanning ?? false) ? 5_000 : 30_000)
    return () => clearInterval(interval)
  }, [refresh, status?.is_scanning, giveUp])

  return { status, loading, error, scan, rebuild, refresh }
}
