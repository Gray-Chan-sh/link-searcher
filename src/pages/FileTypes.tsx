import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'

interface FileTypeInfo {
  extension: string
  name: string
  dependency_met: boolean
  install_guide: string
  count_in_dirs: number
}

export default function FileTypes() {
  const [types, setTypes] = useState<FileTypeInfo[]>([])
  const [loading, setLoading] = useState(true)
  const [filter, setFilter] = useState<'all' | 'missing' | 'has_files'>('all')

  useEffect(() => {
    invoke<FileTypeInfo[]>('get_file_type_support')
      .then(setTypes)
      .catch(() => {})
      .finally(() => setLoading(false))
  }, [])

  const filtered = types.filter(t => {
    if (filter === 'missing') return !t.dependency_met
    if (filter === 'has_files') return t.count_in_dirs > 0
    return true
  })

  return (
    <div className="h-full p-6 overflow-y-auto">
      <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100 mb-1">Supported File Types</h2>
      <p className="text-sm text-gray-500 dark:text-gray-400 mb-4">Check dependency status and file counts per type</p>

      <div className="flex gap-2 mb-4">
        {(['all', 'missing', 'has_files'] as const).map(f => (
          <button key={f} onClick={() => setFilter(f)}
            className={`px-3 py-1 text-xs rounded-full transition-colors ${
              filter === f
                ? 'bg-blue-600 text-white'
                : 'bg-gray-100 dark:bg-gray-800 text-gray-600 dark:text-gray-400 hover:bg-gray-200 dark:hover:bg-gray-700'
            }`}
          >
            {f === 'all' ? 'All' : f === 'missing' ? 'Missing Dependencies' : 'Has Files'}
          </button>
        ))}
      </div>

      {loading && <div className="text-sm text-gray-500">Loading...</div>}

      <div className="space-y-1">
        {filtered.map(t => (
          <div key={t.extension} className="flex items-center gap-3 px-3 py-2 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg text-sm">
            <span className="font-mono font-medium w-16 text-gray-900 dark:text-gray-100">.{t.extension}</span>
            <span className="flex-1 text-gray-600 dark:text-gray-400">{t.name}</span>
            <span className={`text-xs font-medium ${t.dependency_met ? 'text-green-600' : 'text-red-500'}`}>
              {t.dependency_met ? '✓' : '✗'}
            </span>
            <span className="text-xs text-gray-500 w-16 text-right">{t.count_in_dirs} files</span>
            {!t.dependency_met && t.install_guide && (
              <span className="text-xs text-amber-600 dark:text-amber-400 whitespace-pre-wrap">{t.install_guide}</span>
            )}
          </div>
        ))}
      </div>
    </div>
  )
}
