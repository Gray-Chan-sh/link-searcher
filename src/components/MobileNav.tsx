import { NavLink } from 'react-router-dom'
import { useI18n } from '../i18n'
import { SearchIcon, FolderIcon, ActivityIcon, GearIcon } from '../icons'

const NAV_ITEMS = [
  { to: '/', labelKey: 'search', icon: SearchIcon },
  { to: '/browse', labelKey: 'browse', icon: FolderIcon },
  { to: '/index', labelKey: 'index_status', icon: ActivityIcon },
  { to: '/settings', labelKey: 'settings', icon: GearIcon },
] as const

export default function MobileNav() {
  const { t } = useI18n()

  return (
    <nav className="fixed bottom-0 left-0 right-0 z-40 flex items-stretch border-t border-gray-200 dark:border-gray-800 bg-white dark:bg-gray-950 md:hidden pb-[env(safe-area-inset-bottom)]">
      {NAV_ITEMS.map(({ to, labelKey, icon: Icon }) => (
        <NavLink
          key={to}
          to={to}
          end={to === '/'}
          className={({ isActive }) =>
            `flex-1 flex flex-col items-center justify-center gap-0.5 py-2 text-[10px] font-medium transition-colors min-h-[52px] ${
              isActive
                ? 'text-blue-600 dark:text-blue-400'
                : 'text-gray-400 dark:text-gray-500 hover:text-gray-600 dark:hover:text-gray-300'
            }`
          }
        >
          <Icon className="size-5 shrink-0" />
          <span>{t(labelKey)}</span>
        </NavLink>
      ))}
    </nav>
  )
}
