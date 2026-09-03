import { useEffect, useRef, useState } from 'react'
import { Routes, Route, NavLink } from 'react-router-dom'
import { useTheme } from './theme'
import { useI18n } from './i18n'
import { confirm, alert, isTauri, getToken, setToken } from './utils/platform'
import { invoke } from './api/client'
import { checkDependencies, installFunasr } from './api/settings'
import {
  SearchIcon, FolderIcon, ActivityIcon, GearIcon, FileTextIcon,
  SunIcon, MoonIcon, MonitorIcon,
} from './icons'
import { getSettings, updateSettings } from './api/settings'
import SearchPage from './pages/SearchPage'
import DirManager from './pages/DirManager'
import IndexStatus from './pages/IndexStatus'
import Settings from './pages/Settings'
import LogViewer from './pages/LogViewer'
import FileTypes from './pages/FileTypes'
import Browse from './pages/Browse'
import AiChat from './pages/AiChat'
import StatusBar from './components/StatusBar'
import OnboardingWizard from './components/OnboardingWizard'
import ToastContainer from './components/ToastContainer'
import { ErrorBoundary } from './components/ErrorBoundary'

export default function App() {
  const { theme, setTheme } = useTheme()
  const { t } = useI18n()
  const [showOnboarding, setShowOnboarding] = useState(false)
  const [showTokenDialog, setShowTokenDialog] = useState(false)
  const [tokenInput, setTokenInput] = useState('')
  const funasrCheckedRef = useRef(false)

  useEffect(() => {
    const urlToken = new URLSearchParams(window.location.search).get('token')
    if (urlToken) {
      setToken(urlToken)
      const url = new URL(window.location.href)
      url.searchParams.delete('token')
      window.history.replaceState({}, '', url.toString())
    }
  }, [])

  const needsToken = !isTauri() && !getToken()
  const [authFailed, setAuthFailed] = useState(false)

  useEffect(() => {
    if (isTauri()) return
    const onAuthFailed = () => setAuthFailed(true)
    window.addEventListener('auth-failed', onAuthFailed)
    if (!getToken()) return
    import('./api/settings')
      .then(m => m.getSettings())
      .then(() => setAuthFailed(false))
      .catch(() => setAuthFailed(true))
    return () => window.removeEventListener('auth-failed', onAuthFailed)
  }, [])
  const handleSaveToken = async () => {
    const trimmed = tokenInput.trim()
    if (!trimmed) return
    setToken(trimmed)
    try {
      await invoke('update_token', { token: trimmed })
      setShowTokenDialog(false)
      setAuthFailed(false)
      window.location.reload()
    } catch (e) {
      alert(e instanceof Error ? e.message : 'Token 更新失败')
    }
  }

  const navItems = [
    { to: '/', label: t('search'), icon: SearchIcon },
    { to: '/chat', label: t('ai_chat'), icon: FileTextIcon },
    { to: '/browse', label: t('browse'), icon: FolderIcon },
    { to: '/directories', label: t('directories'), icon: FolderIcon },
    { to: '/index', label: t('index_status'), icon: ActivityIcon },
    { to: '/logs', label: t('logs'), icon: FileTextIcon },
    { to: '/file-types', label: t('file_types'), icon: FileTextIcon },
    { to: '/settings', label: t('settings'), icon: GearIcon },
  ] as const

  useEffect(() => {
    if (localStorage.getItem('onboarding_completed') === 'true') return
    getSettings().then(s => {
      if (s['onboarding_done'] !== 'true') {
        setShowOnboarding(true)
      }
    }).catch(() => {
      if (localStorage.getItem('onboarding_completed') !== 'true') {
        setShowOnboarding(true)
      }
    })
  }, [])

  // MCP plugin — bridge DOM events for AI agent interaction
  useEffect(() => {
    if (isTauri()) {
      import('tauri-plugin-mcp').then(({ setupPluginListeners }) => {
        setupPluginListeners().catch(() => {})
      })
    }
  }, [])

  useEffect(() => {
    const checked = sessionStorage.getItem('funasr_prompt_skipped') === '1'
    if (checked || funasrCheckedRef.current) return
    funasrCheckedRef.current = true
    checkDependencies().then(async deps => {
      const funasr = deps.find(d => d.name.includes('FunASR'))
      if (!funasr || funasr.available) return
      const confirmed = await confirm(t('confirm_install_funasr'), t('funasr_install_prompt'))
      if (confirmed) {
        await installFunasr().catch(e => console.error('FunASR install failed:', e))
      } else {
        sessionStorage.setItem('funasr_prompt_skipped', '1')
      }
    }).catch(() => {})
  }, [])

  const handleOnboardingClose = () => {
    setShowOnboarding(false)
    localStorage.setItem('onboarding_completed', 'true')
    updateSettings({ onboarding_done: 'true' }).catch(() => {})
  }

  const cycleTheme = () => {
    setTheme(theme === 'light' ? 'dark' : theme === 'dark' ? 'system' : 'light')
  }

  const themeLabel = theme === 'light' ? t('light') : theme === 'dark' ? t('dark') : t('system')

  return (
    <ErrorBoundary>
      <div className="flex h-screen overflow-hidden bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100">
      <aside className="flex flex-col w-56 border-r border-gray-200 dark:border-gray-800 bg-gray-50 dark:bg-gray-900 shrink-0">
        <div className="px-5 py-4 border-b border-gray-200 dark:border-gray-800">
          <h1 className="text-lg font-semibold tracking-tight">{t('app_name')}</h1>
        </div>
        <nav className="flex-1 px-2 py-3 space-y-1">
          {navItems.map(({ to, label, icon: Icon }) => (
            <NavLink
              key={to}
              to={to}
              end={to === '/'}
              className={({ isActive }) =>
                `flex items-center gap-3 px-3 py-2 rounded-lg text-sm font-medium transition-colors ${
                  isActive
                    ? 'bg-gray-200 dark:bg-gray-800 text-gray-900 dark:text-gray-100'
                    : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-100 hover:bg-gray-100 dark:hover:bg-gray-800/50'
                }`
              }
            >
              <Icon className="size-4 shrink-0" />
              {label}
            </NavLink>
          ))}
        </nav>
        <div className="px-3 py-3 border-t border-gray-200 dark:border-gray-800">
          <button
            onClick={cycleTheme}
            className="flex items-center gap-3 w-full px-3 py-2 rounded-lg text-sm font-medium text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-100 hover:bg-gray-100 dark:hover:bg-gray-800/50 transition-colors"
          >
            {theme === 'light' && <SunIcon className="size-4 shrink-0" />}
            {theme === 'dark' && <MoonIcon className="size-4 shrink-0" />}
            {theme === 'system' && <MonitorIcon className="size-4 shrink-0" />}
            {themeLabel}
          </button>
          {!isTauri() && (
            <button
              onClick={() => { setTokenInput(getToken()); setShowTokenDialog(true) }}
              className="flex items-center gap-3 w-full px-3 py-2 rounded-lg text-sm font-medium text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-100 hover:bg-gray-100 dark:hover:bg-gray-800/50 transition-colors"
            >
              <span className="text-base">🔑</span>
              Token
            </button>
          )}
        </div>
      </aside>

      <div className="flex flex-col flex-1 min-w-0">
        <main className="flex-1 overflow-auto">
          <Routes>
            <Route index element={<SearchPage />} />
            <Route path="browse" element={<Browse />} />
            <Route path="directories" element={<DirManager />} />
            <Route path="index" element={<IndexStatus />} />
            <Route path="logs" element={<LogViewer />} />
            <Route path="file-types" element={<FileTypes />} />
            <Route path="settings" element={<Settings />} />
            <Route path="chat" element={<AiChat />} />
          </Routes>
        </main>
        <StatusBar />
      </div>

      {showOnboarding && <OnboardingWizard onClose={handleOnboardingClose} />}

      <ToastContainer />

      {showTokenDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={() => setShowTokenDialog(false)}>
          <div className="bg-white dark:bg-gray-800 rounded-xl p-6 shadow-xl max-w-sm w-full mx-4" onClick={e => e.stopPropagation()}>
            <h2 className="text-lg font-semibold mb-3 text-gray-900 dark:text-gray-100">Bearer Token</h2>
            <p className="text-sm text-gray-500 dark:text-gray-400 mb-3">修改 Token 后页面将自动刷新</p>
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
              <button onClick={() => setShowTokenDialog(false)} className="px-4 py-2 text-sm text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200">取消</button>
              <button onClick={() => { handleSaveToken(); setAuthFailed(false) }} className="px-4 py-2 text-sm bg-blue-600 text-white rounded-lg hover:bg-blue-700">保存并刷新</button>
            </div>
          </div>
        </div>
      )}

      {(needsToken || authFailed) && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="bg-white dark:bg-gray-800 rounded-xl p-6 shadow-xl max-w-sm w-full mx-4">
            <h2 className="text-lg font-semibold mb-3 text-gray-900 dark:text-gray-100">
              {authFailed ? 'Token 无效' : '请输入 Token'}
            </h2>
            <p className="text-sm text-gray-500 dark:text-gray-400 mb-3">
              {authFailed ? '当前 Token 已失效，请输入正确的 Token' : '访问 Web API 需要认证 Token'}
            </p>
            <input
              type="text"
              value={tokenInput}
              onChange={e => setTokenInput(e.target.value)}
              onKeyDown={e => { if (e.key === 'Enter') handleSaveToken() }}
              placeholder="输入 Bearer Token"
              className="w-full px-3 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg bg-gray-50 dark:bg-gray-900 text-gray-900 dark:text-gray-100 font-mono focus:outline-none focus:ring-2 focus:ring-blue-500 mb-4"
              autoFocus
            />
            <div className="flex justify-end">
              <button onClick={() => { handleSaveToken(); setAuthFailed(false) }} className="px-4 py-2 text-sm bg-blue-600 text-white rounded-lg hover:bg-blue-700">确认</button>
            </div>
          </div>
        </div>
      )}
    </div>
    </ErrorBoundary>
  )
}
