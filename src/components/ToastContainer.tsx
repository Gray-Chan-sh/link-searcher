import { useEffect, useState } from 'react'
import { subscribeToasts, dismissToast, type ToastItem } from '../utils/toast'

const TYPE_STYLE: Record<ToastItem['type'], string> = {
  success: 'border-green-500/40 text-green-700 dark:text-green-300',
  error: 'border-red-500/40 text-red-600 dark:text-red-300',
  info: 'border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-300',
}

const TYPE_ICON: Record<ToastItem['type'], string> = {
  success: '✓',
  error: '✕',
  info: 'ℹ',
}

export default function ToastContainer() {
  const [toasts, setToasts] = useState<ToastItem[]>([])

  useEffect(() => subscribeToasts(setToasts), [])

  return (
    <div className="fixed bottom-4 right-4 z-[100] flex flex-col gap-2 items-end pointer-events-none">
      {toasts.map(t => (
        <button
          key={t.id}
          onClick={() => dismissToast(t.id)}
          className={`pointer-events-auto flex items-center gap-2 px-3 py-2 rounded-lg shadow-lg border bg-white dark:bg-gray-800 text-sm max-w-xs text-left transition-all ${TYPE_STYLE[t.type]}`}
          role="status"
        >
          <span className="shrink-0">{TYPE_ICON[t.type]}</span>
          <span className="break-words">{t.message}</span>
        </button>
      ))}
    </div>
  )
}
