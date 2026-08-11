import { useCallback, useEffect, useRef, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { ask } from '@tauri-apps/plugin-dialog'
import { getLogs, clearLogs } from '../api/logs'
import { useI18n } from '../i18n'
import { LoadingSpinner, RefreshIcon } from '../icons'

type Filter = 'all' | 'index' | 'ocr' | 'scan' | 'search' | 'error'

const FILTERS: { value: Filter; labelKey: string }[] = [
  { value: 'all', labelKey: 'all' },
  { value: 'index', labelKey: 'index' },
  { value: 'ocr', labelKey: 'ocr' },
  { value: 'scan', labelKey: 'scan' },
  { value: 'search', labelKey: 'search' },
  { value: 'error', labelKey: 'error' },
]

function filterLogs(logs: string[], filter: Filter, grep: string): string[] {
  if (filter === 'error') {
    return logs.filter(l => l.includes('ERROR') || l.includes('error'))
  }
  const tag = filter !== 'all' ? `[${filter.toUpperCase()}]` : ''
  return logs.filter(l =>
    (filter === 'all' || l.includes(tag)) &&
    (grep === '' || l.toLowerCase().includes(grep.toLowerCase())),
  )
}

function logKey(line: string, i: number): string {
  return `${line.slice(0, 24).replace(/\s/g, '-')}-${i}`
}

function logLineColor(line: string): string {
  if (line.includes('ERROR') || line.includes('error')) return 'text-red-600 dark:text-red-400'
  if (line.includes('ocr')) return 'text-purple-600 dark:text-purple-400'
  if (line.includes('dedup')) return 'text-amber-600 dark:text-amber-400'
  return 'text-gray-700 dark:text-gray-300'
}

export default function LogViewer() {
  const { t } = useI18n()
  const [searchParams] = useSearchParams()
  const [logs, setLogs] = useState<string[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [filter, setFilter] = useState<Filter>('all')
  const [grep, setGrep] = useState<string>(searchParams.get('q') ?? '')
  const [autoScroll, setAutoScroll] = useState<boolean>(!searchParams.get('q'))
  const [pendingNewLogs, setPendingNewLogs] = useState(false)

  const bottomRef = useRef<HTMLDivElement>(null)
  const containerRef = useRef<HTMLDivElement>(null)
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const prevLogCountRef = useRef(0)

  const fetchLogs = useCallback(async () => {
    try {
      setError(null)
      const result = await getLogs(500)
      setLogs(result)
    } catch (e) {
      setError(e instanceof Error ? e.message : t('failed_load_logs'))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    fetchLogs()
    intervalRef.current = setInterval(fetchLogs, 3000)
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current)
    }
  }, [fetchLogs])

  useEffect(() => {
    if (!autoScroll) {
      if (logs.length > prevLogCountRef.current && prevLogCountRef.current > 0) {
        setPendingNewLogs(true)
      }
    } else {
      setPendingNewLogs(false)
      bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
    }
    prevLogCountRef.current = logs.length
  }, [logs, autoScroll])

  const handleClear = async () => {
    const confirmed = await ask(t('confirm_clear_logs'), { title: t('clear'), kind: 'warning' })
    if (!confirmed) return
    try {
      await clearLogs()
      setLogs([])
      prevLogCountRef.current = 0
    } catch (e) {
      setError(e instanceof Error ? e.message : t('failed_clear_logs'))
    }
  }

  const scrollToBottom = () => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
    setPendingNewLogs(false)
  }

  const filteredLogs = filterLogs(logs, filter, grep)

  return (
    <div className="h-full flex flex-col p-6">
      <div className="flex items-center justify-between mb-4">
        <div>
          <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">{t('logs')}</h2>
          <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
            {t('log_activity')}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={fetchLogs}
            className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-gray-700 dark:text-gray-300 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
          >
            <RefreshIcon className="size-4" />
            {t('refresh')}
          </button>
          <button
            onClick={handleClear}
            className="px-3 py-2 text-sm font-medium text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-900 rounded-lg hover:bg-red-100 dark:hover:bg-red-900/40 transition-colors"
          >
            {t('clear')}
          </button>
        </div>
      </div>

      <div className="flex items-center gap-2 mb-3">
        {FILTERS.map(f => (
          <button
            key={f.value}
            onClick={() => setFilter(f.value)}
            className={`px-3 py-1 text-xs font-medium rounded-full border transition-colors ${
              filter === f.value
                ? 'bg-blue-500 text-white border-blue-500'
                : 'text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border-gray-200 dark:border-gray-700 hover:bg-gray-100 dark:hover:bg-gray-700'
            }`}
          >
            {t(f.labelKey)}
          </button>
        ))}
        <input
          type="text"
          value={grep}
          onChange={e => setGrep(e.target.value)}
          placeholder={t('grep_logs')}
          className="px-2 py-1 text-xs bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500 transition-colors"
        />
        <button
          onClick={() => {
            setAutoScroll(prev => {
              if (!prev) setPendingNewLogs(false)
              return !prev
            })
          }}
          className={`px-3 py-1 text-xs font-medium rounded-full border transition-colors ${
            autoScroll
              ? 'bg-green-500 text-white border-green-500'
              : 'text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-800 border-gray-200 dark:border-gray-700 hover:bg-gray-100 dark:hover:bg-gray-700'
          }`}
        >
          {t('auto_scroll')} {autoScroll ? t('on') : t('off')}
        </button>
      </div>

      {error && (
        <div className="px-4 py-3 mb-4 text-sm text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-900 rounded-lg">
          {error}
        </div>
      )}

      {loading && (
        <div className="flex items-center justify-center py-16">
          <LoadingSpinner className="size-6 text-blue-500" />
        </div>
      )}

      {!loading && filteredLogs.length === 0 && (
        <div className="flex flex-col items-center justify-center py-16 text-center">
          <p className="text-sm text-gray-500 dark:text-gray-400">
            {logs.length === 0 ? t('no_log_entries') : t('no_logs_match')}
          </p>
          <p className="text-xs text-gray-400 dark:text-gray-500 mt-1">
            {logs.length === 0
              ? t('logs_appear_hint')
              : t('try_other_filter')}
          </p>
        </div>
      )}

      {!loading && filteredLogs.length > 0 && (
        <div className="flex-1 relative min-h-0">
          <div
            ref={containerRef}
            className="h-full overflow-y-auto bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg p-4 font-mono text-xs leading-relaxed"
          >
            {filteredLogs.map((line, i) => (
              <div key={logKey(line, i)} className={`whitespace-pre-wrap break-all ${logLineColor(line)}`}>
                {line}
              </div>
            ))}
            <div ref={bottomRef} />
          </div>
          {pendingNewLogs && (
            <button
              onClick={scrollToBottom}
              className="absolute bottom-3 right-3 px-3 py-1.5 text-xs font-medium text-white bg-blue-500 rounded-full shadow-lg hover:bg-blue-600 transition-colors animate-pulse"
            >
              ↓ {t('new_logs')}
            </button>
          )}
        </div>
      )}
    </div>
  )
}