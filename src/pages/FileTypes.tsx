import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useI18n } from '../i18n'

interface FileTypeInfo {
  extension: string
  name: string
  dependency_met: boolean
  install_guide: string
  count_in_dirs: number
}

interface UnsupportedExtInfo {
  extension: string
  count: number
  dir_id: string
  rescusable: boolean
  hint: string
}

export default function FileTypes() {
  const { t } = useI18n()
  const [types, setTypes] = useState<FileTypeInfo[]>([])
  const [unsupported, setUnsupported] = useState<UnsupportedExtInfo[]>([])
  const [loading, setLoading] = useState(true)
  const [filter, setFilter] = useState<'all' | 'missing' | 'has_files'>('all')

  useEffect(() => {
    invoke<FileTypeInfo[]>('get_file_type_support')
      .then(setTypes)
      .catch(() => {})
    invoke<UnsupportedExtInfo[]>('get_unsupported_ext_stats')
      .then(setUnsupported)
      .catch(() => {})
      .finally(() => setLoading(false))
  }, [])

  const filtered = types.filter(ft => {
    if (filter === 'missing') return !ft.dependency_met
    if (filter === 'has_files') return ft.count_in_dirs > 0
    return true
  })

  return (
    <div className="h-full p-6 overflow-y-auto">
      <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100 mb-1">{t('supported_file_types')}</h2>
      <p className="text-sm text-gray-500 dark:text-gray-400 mb-4">{t('file_types_desc')}</p>

      <div className="flex gap-2 mb-4">
        {(['all', 'missing', 'has_files'] as const).map(f => (
          <button key={f} onClick={() => setFilter(f)}
            className={`px-3 py-1 text-xs rounded-full transition-colors ${
              filter === f
                ? 'bg-blue-600 text-white'
                : 'bg-gray-100 dark:bg-gray-800 text-gray-600 dark:text-gray-400 hover:bg-gray-200 dark:hover:bg-gray-700'
            }`}
          >
            {f === 'all' ? t('all') : f === 'missing' ? t('missing_dependencies') : t('has_files')}
          </button>
        ))}
      </div>

      {loading && <div className="text-sm text-gray-500">{t('loading')}</div>}

      <div className="space-y-1">
        {filtered.map(ft => (
          <div key={ft.extension} className="flex items-center gap-3 px-3 py-2 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg text-sm">
            <span className="font-mono font-medium w-16 text-gray-900 dark:text-gray-100">.{ft.extension}</span>
            <span className="flex-1 text-gray-600 dark:text-gray-400">{ft.name}</span>
            <span className={`text-xs font-medium ${ft.dependency_met ? 'text-green-600' : 'text-red-500'}`}>
              {ft.dependency_met ? '✓' : '✗'}
            </span>
            <span className="text-xs text-gray-500 w-16 text-right">{ft.count_in_dirs} {t('files')}</span>
            {!ft.dependency_met && ft.install_guide && (
              <span className="text-xs text-amber-600 dark:text-amber-400 whitespace-pre-wrap">{ft.install_guide}</span>
            )}
          </div>
        ))}
      </div>

      {unsupported.length > 0 && (
        <div className="mt-8">
          <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100 mb-1">{t('unsupported_file_types')}</h3>
          <p className="text-xs text-gray-500 dark:text-gray-400 mb-3">{t('unsupported_file_types_desc')}</p>
          <div className="space-y-1">
            {unsupported.map(ue => (
              <div key={ue.extension} className="flex items-center gap-3 px-3 py-2 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg text-sm">
                <span className="font-mono font-medium w-24 text-gray-900 dark:text-gray-100">.{ue.extension}</span>
                <span className={`text-xs font-medium w-20 ${ue.rescusable ? 'text-amber-500' : 'text-red-500'}`}>
                  {ue.rescusable ? t('missing_dependencies') : t('unsupported')}
                </span>
                <span className="text-xs text-gray-500 w-16 text-right">{ue.count} {t('files')}</span>
                <span className="flex-1 text-xs text-gray-400 dark:text-gray-500">{ue.hint}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  )
}