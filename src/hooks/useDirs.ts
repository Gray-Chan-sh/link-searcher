import { useCallback, useEffect, useState } from 'react'
import { listDirs, addDir, removeDir, type DirConfig } from '../api/dirs'
import { triggerScan } from '../api/index'
import { open } from '@tauri-apps/plugin-dialog'

interface UseDirsReturn {
  dirs: DirConfig[]
  loading: boolean
  error: string | null
  addDirectory: () => Promise<void>
  removeDirectory: (id: string) => Promise<void>
  refresh: () => Promise<void>
}

export function useDirs(): UseDirsReturn {
  const [dirs, setDirs] = useState<DirConfig[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    try {
      setError(null)
      setLoading(true)
      const result = await listDirs()
      setDirs(result)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to list directories')
    } finally {
      setLoading(false)
    }
  }, [])

  const addDirectory = useCallback(async () => {
    try {
      const selected = await open({ directory: true, multiple: false, title: 'Select Directory' })
      if (selected) {
        const path = typeof selected === 'string' ? selected : selected
        await addDir(path, undefined, true)
        await refresh()
        await triggerScan()
      }
    } catch (e) {
      setError(String(e))
    }
  }, [refresh])

  const removeDirectory = useCallback(async (id: string) => {
    try {
      await removeDir(id)
      await refresh()
    } catch (e) {
      setError(String(e))
    }
  }, [refresh])

  useEffect(() => {
    refresh()
  }, [refresh])

  return { dirs, loading, error, addDirectory, removeDirectory, refresh }
}
