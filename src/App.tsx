import { useEffect, useRef, useState } from 'react'
import { Routes, Route, NavLink } from 'react-router-dom'
import { ask } from '@tauri-apps/plugin-dialog'
import { useTheme } from './theme'
import { useI18n } from './i18n'
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
import StatusBar from './components/StatusBar'
import OnboardingWizard from './components/OnboardingWizard'
import { ErrorBoundary } from './components/ErrorBoundary'

export default function App() {
  const { theme, setTheme } = useTheme()
  const { t } = useI18n()
  const [showOnboarding, setShowOnboarding] = useState(false)
  const funasrCheckedRef = useRef(false)

  const navItems = [
    { to: '/', label: t('search'), icon: SearchIcon },
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

  useEffect(() => {
    const checked = sessionStorage.getItem('funasr_prompt_skipped') === '1'
    if (checked || funasrCheckedRef.current) return
    funasrCheckedRef.current = true
    checkDependencies().then(async deps => {
      const funasr = deps.find(d => d.name.includes('FunASR'))
      if (!funasr || funasr.available) return
      const confirmed = await ask(t('confirm_install_funasr'), {
        title: t('funasr_install_prompt'),
        kind: 'warning',
        okLabel: t('install_now'),
        cancelLabel: t('not_now'),
      })
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
          </Routes>
        </main>
        <StatusBar />
      </div>

      {showOnboarding && <OnboardingWizard onClose={handleOnboardingClose} />}
    </div>
    </ErrorBoundary>
  )
}
