import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useI18n } from '../i18n'
import { isTauri, getToken, setToken } from '../utils/platform'
import { invoke } from '../api/client'
import { SunIcon, MoonIcon, MonitorIcon } from '../icons'

interface MobileHeaderProps {
  theme: 'light' | 'dark' | 'system'
  cycleTheme: () => void
}

export default function MobileHeader({ theme, cycleTheme }: MobileHeaderProps) {
  const { t } = useI18n()
  const navigate = useNavigate()
  const [showMenu, setShowMenu] = useState(false)
  const [showTokenDialog, setShowTokenDialog] = useState(false)
  const [tokenInput, setTokenInput] = useState('')

  const themeLabel = theme === 'light' ? t('light') : theme === 'dark' ? t('dark') : t('system')

  const handleSaveToken = async () => {
    const trimmed = tokenInput.trim()
    if (!trimmed) return
    setToken(trimmed)
    try {
      await invoke('update_token', { token: trimmed })
      setShowTokenDialog(false)
      window.location.reload()
    } catch {
      // silent
    }
  }

  return (
    <>
      <header className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-800 bg-white dark:bg-gray-950 lg:hidden shrink-0">
        <h1
          className="text-base font-semibold tracking-tight cursor-pointer"
          onClick={() => { navigate('/'); setShowMenu(false) }}
        >
          {t('app_name')}
        </h1>
        <button
          onClick={() => setShowMenu(v => !v)}
          className="p-2 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors"
          aria-label="Menu"
        >
          <svg className="size-5 text-gray-500 dark:text-gray-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <line x1="4" x2="20" y1="12" y2="12" />
            <line x1="4" x2="20" y1="6" y2="6" />
            <line x1="4" x2="20" y1="18" y2="18" />
          </svg>
        </button>
      </header>

      {/* Dropdown menu */}
      {showMenu && (
        <div className="fixed inset-0 z-50 lg:hidden" onClick={() => setShowMenu(false)}>
          <div className="absolute right-2 top-14 w-52 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl shadow-xl overflow-hidden">
            <button
              onClick={() => { cycleTheme(); setShowMenu(false) }}
              className="flex items-center gap-3 w-full px-4 py-3 text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-700 transition-colors"
            >
              {theme === 'light' && <SunIcon className="size-4" />}
              {theme === 'dark' && <MoonIcon className="size-4" />}
              {theme === 'system' && <MonitorIcon className="size-4" />}
              <span>{themeLabel}</span>
            </button>
            {!isTauri() && (
              <button
                onClick={() => { setTokenInput(getToken()); setShowTokenDialog(true); setShowMenu(false) }}
                className="flex items-center gap-3 w-full px-4 py-3 text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-700 transition-colors"
              >
                <span className="text-base">🔑</span>
                <span>Token</span>
              </button>
            )}
            <button
              onClick={() => { navigate('/settings'); setShowMenu(false) }}
              className="flex items-center gap-3 w-full px-4 py-3 text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-700 transition-colors border-t border-gray-100 dark:border-gray-700"
            >
              <span className="text-base">⚙️</span>
              <span>{t('settings')}</span>
            </button>
          </div>
        </div>
      )}

      {/* Token dialog */}
      {showTokenDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={() => setShowTokenDialog(false)}>
          <div className="bg-white dark:bg-gray-800 rounded-xl p-6 shadow-xl max-w-sm w-full mx-4" onClick={e => e.stopPropagation()}>
            <h2 className="text-lg font-semibold mb-3 text-gray-900 dark:text-gray-100">Bearer Token</h2>
            <input
              type="text"
              value={tokenInput}
              onChange={e => setTokenInput(e.target.value)}
              onKeyDown={e => { if (e.key === 'Enter') handleSaveToken() }}
              placeholder="输入新的 Token"
              className="w-full px-3 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg bg-gray-50 dark:bg-gray-900 text-gray-900 dark:text-gray-100 font-mono focus:outline-none focus:ring-2 focus:ring-blue-500 mb-4"
              autoFocus
            />
            <div className="flex justify-end gap-2">
              <button onClick={() => setShowTokenDialog(false)} className="px-4 py-2 text-sm text-gray-500">取消</button>
              <button onClick={handleSaveToken} className="px-4 py-2 text-sm bg-blue-600 text-white rounded-lg hover:bg-blue-700">保存并刷新</button>
            </div>
          </div>
        </div>
      )}
    </>
  )
}
