import { useEffect, useMemo, useRef, useState } from 'react'
import { useI18n } from '../../i18n'
import { useSetup } from '../../hooks/useSetup'
import { LoadingSpinner } from '../../icons'
import { Section } from './SettingsFields'
import { listen } from '../../api/client'
import { setActiveModel } from '../../api/config'
import type { DepStatus } from '../../api/settings'

function fmtSize(bytes: number): string {
  if (bytes <= 0) return ''
  const mb = bytes / (1024 * 1024)
  return mb < 1024 ? `${mb.toFixed(0)} MB` : `${(mb / 1024).toFixed(1)} GB`
}

function fmtBytes(bytes: number): string {
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

/// A single-choice group of local models rendered as one row with a dropdown.
interface ModelGroup {
  /// Dep ids (== model directory names).
  ids: string[]
  /// Short dropdown labels keyed by dep id.
  labels: Record<string, string>
  titleKey: string
  /// Active-model role this group installs into.
  kind: 'embedding' | 'reranker'
  /// Default selection (dep id).
  defaultId: string
  recommended?: boolean
}

const GROUPS: ModelGroup[] = [
  {
    ids: ['bge-small', 'bge-base', 'bge-large'],
    labels: {
      'bge-small': '512 维 · 快 · 约 95MB',
      'bge-base': '768 维 · 约 407MB',
      'bge-large': '1024 维 · 约 1.2GB',
    },
    titleKey: 'bge_local_models',
    kind: 'embedding',
    defaultId: 'bge-large',
    recommended: true,
  },
  {
    ids: ['bge-reranker-base', 'ms-marco-MiniLM-L-6-v2'],
    labels: {
      'bge-reranker-base': 'BGE-Reranker-Base · 中英 · 约 1.1GB',
      'ms-marco-MiniLM-L-6-v2': 'MS-Marco MiniLM-L6 · 英文 · 约 90MB',
    },
    titleKey: 'rerank_local_models',
    kind: 'reranker',
    defaultId: 'bge-reranker-base',
  },
]

/** Dependency center: full list of installable runtime deps with progress. */
export function DepsTab() {
  const { t } = useI18n()
  const setup = useSetup()
  const deps = useMemo(() => setup.status?.deps ?? [], [setup.status])
  const [sel, setSel] = useState<Record<string, string>>(() =>
    Object.fromEntries(GROUPS.map(g => [g.kind, g.defaultId])),
  )
  // Which grouped model to activate once its background install completes.
  const pendingRef = useRef<{ kind: 'embedding' | 'reranker'; id: string } | null>(null)

  useEffect(() => {
    const un = listen<{ dep: string; success: boolean; message: string }>(
      'dep-install-done',
      r => {
        const pending = pendingRef.current
        pendingRef.current = null
        // Make the chosen model active; the backend also prunes the sibling
        // models of the same group (set_active_model).
        if (r.success && pending && pending.id === r.dep) {
          setActiveModel(pending.kind, `local:${r.dep}`).catch(() => {})
        }
      },
    )
    return () => {
      un.then(f => f())
    }
  }, [])

  if (setup.loading) {
    return (
      <div className="flex items-center justify-center h-40">
        <LoadingSpinner className="size-6 text-blue-500" />
      </div>
    )
  }

  const active = setup.activeDep

  const depRow = (dep: DepStatus) => {
    const installing = active === dep.id
    const pct =
      installing && setup.progress && dep.size_bytes > 0
        ? Math.min(100, Math.round((setup.progress.bytes / dep.size_bytes) * 100))
        : null
    return (
      <div key={dep.id} className="flex items-start gap-3 p-3 rounded-lg border border-gray-200 dark:border-gray-700">
        <div className="mt-0.5 text-lg">
          {dep.available ? (
            <span className="text-green-600 dark:text-green-400">✓</span>
          ) : installing ? (
            <LoadingSpinner className="size-5 text-blue-500" />
          ) : (
            <span className="text-amber-500">✗</span>
          )}
        </div>
        <div className="flex-1 min-w-0">
          <p className="text-sm text-gray-900 dark:text-gray-100">
            {dep.name}
            {dep.recommended && <span className="ml-2 text-xs text-blue-500">推荐</span>}
          </p>
          <p className="text-xs text-gray-500 mt-0.5">
            {dep.hint}
            {dep.size_bytes > 0 && ` · ${fmtSize(dep.size_bytes)}`}
          </p>
          {installing && pct !== null && (
            <div className="mt-2">
              <div className="h-1.5 w-full bg-gray-200 dark:bg-gray-700 rounded-full overflow-hidden">
                <div className="h-full bg-blue-500 transition-all" style={{ width: `${pct}%` }} />
              </div>
              <p className="text-xs text-gray-500 mt-1">
                {setup.progress && fmtBytes(setup.progress.bytes)} · {pct}%
              </p>
            </div>
          )}
          {!dep.available && !installing && setup.lastResult?.dep === dep.id && (
            <p className={`text-xs mt-1 ${setup.lastResult.success ? 'text-green-600' : 'text-red-500'}`}>
              {setup.lastResult.message}
            </p>
          )}
        </div>
        {!dep.available && (
          <button
            onClick={() => {
              if (installing) void setup.cancelInstall()
              else void setup.startInstall(dep.id)
            }}
            disabled={!!active && !installing}
            className="shrink-0 px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg transition-colors"
          >
            {installing ? t('cancel') : t('install_now')}
          </button>
        )}
      </div>
    )
  }

  const groupRow = (group: ModelGroup) => {
    const groupDeps = deps.filter(d => group.ids.includes(d.id))
    if (groupDeps.length === 0) return null
    const selected = groupDeps.find(d => d.id === sel[group.kind]) ?? groupDeps[groupDeps.length - 1]
    if (!selected) return null
    const installing = active === selected.id
    const pct =
      installing && setup.progress && selected.size_bytes > 0
        ? Math.min(100, Math.round((setup.progress.bytes / selected.size_bytes) * 100))
        : null
    return (
      <div key={`group-${group.kind}`} className="flex items-start gap-3 p-3 rounded-lg border border-gray-200 dark:border-gray-700">
        <div className="mt-0.5 text-lg">
          {selected.available ? (
            <span className="text-green-600 dark:text-green-400">✓</span>
          ) : installing ? (
            <LoadingSpinner className="size-5 text-blue-500" />
          ) : (
            <span className="text-amber-500">✗</span>
          )}
        </div>
        <div className="flex-1 min-w-0">
          <p className="text-sm text-gray-900 dark:text-gray-100">
            {t(group.titleKey)}
            {group.recommended && <span className="ml-2 text-xs text-blue-500">推荐</span>}
          </p>
          <div className="mt-1.5 flex flex-wrap items-center gap-2">
            <select
              value={selected.id}
              onChange={e => setSel(s => ({ ...s, [group.kind]: e.target.value }))}
              disabled={!!active || installing}
              className="text-xs rounded-lg border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-700 dark:text-gray-200 px-2 py-1 disabled:opacity-50"
            >
              {group.ids.map(id => {
                const d = groupDeps.find(x => x.id === id)
                if (!d) return null
                return (
                  <option key={id} value={id}>
                    {group.labels[id] ?? id}{d.available ? ' ✓' : ''}
                  </option>
                )
              })}
            </select>
            <span className="text-xs text-gray-500 dark:text-gray-400">{selected.hint}</span>
          </div>
          {installing && pct !== null && (
            <div className="mt-2">
              <div className="h-1.5 w-full bg-gray-200 dark:bg-gray-700 rounded-full overflow-hidden">
                <div className="h-full bg-blue-500 transition-all" style={{ width: `${pct}%` }} />
              </div>
              <p className="text-xs text-gray-500 mt-1">
                {setup.progress && fmtBytes(setup.progress.bytes)} · {pct}%
              </p>
            </div>
          )}
          {!selected.available && !installing && setup.lastResult?.dep === selected.id && (
            <p className={`text-xs mt-1 ${setup.lastResult.success ? 'text-green-600' : 'text-red-500'}`}>
              {setup.lastResult.message}
            </p>
          )}
        </div>
        {!selected.available && (
          <button
            onClick={() => {
              if (installing) {
                void setup.cancelInstall()
              } else {
                pendingRef.current = { kind: group.kind, id: selected.id }
                void setup.startInstall(selected.id)
              }
            }}
            disabled={!!active && !installing}
            className="shrink-0 px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg transition-colors"
          >
            {installing ? t('cancel') : t('install_now')}
          </button>
        )}
      </div>
    )
  }

  const renderedGroups = new Set<string>()

  return (
    <div className="space-y-6">
      <Section title={t('dep_center_title')}>
        <p className="text-sm text-gray-500 dark:text-gray-400 mb-3">{t('dep_center_desc')}</p>
        <p className="text-xs text-gray-400 mb-4">data dir: {setup.status?.data_dir}</p>

        <div className="space-y-2">
          {deps.map(dep => {
            const group = GROUPS.find(g => g.ids.includes(dep.id))
            if (group) {
              if (renderedGroups.has(group.kind)) return null
              renderedGroups.add(group.kind)
              return groupRow(group)
            }
            return depRow(dep)
          })}
        </div>
      </Section>
    </div>
  )
}
