import { useCallback, useEffect, useState } from 'react'
import { getIndexStatus, triggerScan, rebuildIndex, type IndexStatus } from '../api/index'

interface UseIndexStatusReturn {
  status: IndexStatus | null
  loading: boolean
  error: string | null
  scan: (dirId?: string) => Promise<void>
  rebuild: () => Promise<void>
  refresh: () => Promise<void>
}

const POLL_INTERVAL = 5_000

export function useIndexStatus(): UseIndexStatusReturn {
  const [status, setStatus] = useState<IndexStatus | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    try {
      setError(null)
      const s = await getIndexStatus()
      setStatus(s)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to get index status')
    } finally {
      setLoading(false)
    }
  }, [])

  const scan = useCallback(async (dirId?: string) => {
    try {
      await triggerScan(dirId)
      void refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to trigger scan')
    }
  }, [refresh])

  const rebuild = useCallback(async () => {
    try {
      // Immediately show scanning state (rebuildIndex spawns background task)
      setStatus(prev => prev ? { ...prev, indexed: 0, pending: 0, errors: 0, is_scanning: true } : null)
      await rebuildIndex()
      // Wait for background task to start clearing the DB
      await new Promise(r => setTimeout(r, 1000))
      void refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to rebuild index')
    }
  }, [refresh])

  // Adaptive polling: 5s while scanning, 30s when idle
  useEffect(() => {
    refresh()
    const interval = setInterval(refresh, (status?.is_scanning ?? false) ? 5_000 : 30_000)
    return () => clearInterval(interval)
  }, [refresh, status?.is_scanning])

  return { status, loading, error, scan, rebuild, refresh }
}
