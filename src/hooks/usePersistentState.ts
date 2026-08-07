import { useEffect, useState } from 'react'

function loadFromStorage<T>(key: string, fallback: T): T {
  try {
    const item = localStorage.getItem(key)
    return item ? JSON.parse(item) as T : fallback
  } catch (e) {
    console.warn('Failed to load from localStorage', e)
    return fallback
  }
}

function saveToStorage(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value))
  } catch (e) {
    console.warn('Failed to save to localStorage', e)
  }
}

/** useState backed by localStorage — survives tab switches and restarts. */
export function usePersistentState<T>(key: string, initial: T) {
  const [value, setValue] = useState<T>(() => loadFromStorage(key, initial))
  useEffect(() => {
    saveToStorage(key, value)
  }, [key, value])
  return [value, setValue] as const
}