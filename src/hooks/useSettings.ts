import { useCallback, useEffect, useState } from 'react'
import { getSettings, updateSettings } from '../api/settings'

interface UseSettingsReturn {
  settings: Record<string, string>
  loading: boolean
  saving: boolean
  error: string | null
  saveError: string | null
  setValue: (key: string, value: string) => void
  save: () => Promise<void>
  refresh: () => Promise<void>
}

export function useSettings(): UseSettingsReturn {
  const [settings, setSettings] = useState<Record<string, string>>({})
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [saveError, setSaveError] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    try {
      setError(null)
      setLoading(true)
      const result = await getSettings()
      setSettings(result)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load settings')
    } finally {
      setLoading(false)
    }
  }, [])

  const setValue = useCallback((key: string, value: string) => {
    setSettings(prev => ({ ...prev, [key]: value }))
  }, [])

  const save = useCallback(async () => {
    try {
      setSaveError(null)
      setSaving(true)
      await updateSettings(settings)
    } catch (e) {
      setSaveError(e instanceof Error ? e.message : 'Failed to save settings')
    } finally {
      setSaving(false)
    }
  }, [settings])

  useEffect(() => {
    refresh()
  }, [refresh])

  return { settings, loading, saving, error, saveError, setValue, save, refresh }
}
