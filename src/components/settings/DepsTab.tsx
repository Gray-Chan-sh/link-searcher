import { useEffect, useMemo, useRef, useState } from 'react'
import { useI18n } from '../../i18n'
import { useSetup } from '../../hooks/useSetup'
import { LoadingSpinner } from '../../icons'
import { Section } from './SettingsFields'
import { listen } from '../../api/client'
import { getConfig, setActiveModel, updateConfig } from '../../api/config'
import {
  checkDepUpdates,
  rollbackDep,
  upgradeDep,
  type DepStatus,
  type UpdateCheck,
} from '../../api/settings'

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
  // 安装完自动启用的模型会改变向量空间；如果说不出"语料已同空间"，必须提示
  // 用户去重建 —— 这条路径绕过了设置页的切换对话框（用户点的是"下载并使用"）。
  const [switchNote, setSwitchNote] = useState<string | null>(null)
  // 模型在线升级：检查结果 + 进行中的操作提示
  const [updates, setUpdates] = useState<UpdateCheck | null>(null)
  const [checking, setChecking] = useState(false)
  const [upgradeMsg, setUpgradeMsg] = useState<string | null>(null)
  // 后台自动升级开关（持久化在 config.json）
  const [autoUpdate, setAutoUpdate] = useState(false)

  useEffect(() => {
    getConfig()
      .then(c => setAutoUpdate(Boolean(c.auto_update_models)))
      .catch(() => {})
  }, [])

  const toggleAutoUpdate = async (next: boolean) => {
    setAutoUpdate(next)
    try {
      await updateConfig({ auto_update_models: next })
    } catch (e) {
      setAutoUpdate(!next)
      setUpgradeMsg(e instanceof Error ? e.message : String(e))
    }
  }

  const runCheck = async () => {
    setChecking(true)
    try {
      setUpdates(await checkDepUpdates())
    } catch (e) {
      setUpdates({
        ok: false,
        error: e instanceof Error ? e.message : String(e),
        app_version: '',
        updates: [],
      })
    } finally {
      setChecking(false)
    }
  }

  useEffect(() => {
    const un = listen<{ dep: string; success: boolean; message: string }>(
      'dep-install-done',
      r => {
        const pending = pendingRef.current
        pendingRef.current = null
        // Make the chosen model active; the backend also prunes the sibling
        // models of the same group (set_active_model).
        if (r.success && pending && pending.id === r.dep) {
          setActiveModel(pending.kind, `local:${r.dep}`)
            .then(res => {
              setSwitchNote(
                res.rebuild_required
                  ? t('embed_switch_suspended', { model: r.dep })
                  : null,
              )
            })
            .catch(e => setSwitchNote(e instanceof Error ? e.message : String(e)))
        }
      },
    )
    return () => {
      un.then(f => f())
    }
  }, [t])

  if (setup.loading) {
    return (
      <div className="flex items-center justify-center h-40">
        <LoadingSpinner className="size-6 text-blue-500" />
      </div>
    )
  }

  const active = setup.activeDep

  const note = switchNote && (
    <div className="mb-4 px-3 py-2 text-xs rounded-lg bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 text-amber-700 dark:text-amber-300">
      {switchNote}
    </div>
  )

  const doUpgrade = async (dep: string) => {
    setUpgradeMsg(null)
    try {
      await upgradeDep(dep)
    } catch (e) {
      setUpgradeMsg(e instanceof Error ? e.message : String(e))
    }
  }

  const doRollback = async (dep: string) => {
    setUpgradeMsg(null)
    try {
      await rollbackDep(dep)
    } catch (e) {
      setUpgradeMsg(e instanceof Error ? e.message : String(e))
    }
  }

  const depRow = (dep: DepStatus) => {
    const installing = active === dep.id
    const update = updates?.updates.find(u => u.dep === dep.id)
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
            {dep.current_version && (
              <span className="ml-2 text-xs text-gray-400">{dep.current_version}</span>
            )}
          </p>
          <p className="text-xs text-gray-500 mt-0.5">
            {dep.hint}
            {dep.size_bytes > 0 && ` · ${fmtSize(dep.size_bytes)}`}
          </p>
          {update && (
            <p className="text-xs mt-1 text-emerald-600 dark:text-emerald-400">
              可升级到 {update.latest}
              {update.size_bytes > 0 && ` · ${fmtSize(update.size_bytes)}`}
              {update.notes && ` · ${update.notes}`}
            </p>
          )}
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
        {!dep.available ? (
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
        ) : (
          <div className="shrink-0 flex flex-col items-end gap-1">
            {update && (
              <button
                onClick={() => void doUpgrade(dep.id)}
                disabled={!!active}
                className="px-3 py-1.5 text-xs font-medium text-white bg-emerald-600 hover:bg-emerald-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg transition-colors"
              >
                升级
              </button>
            )}
            {dep.previous_version && (
              <button
                onClick={() => void doRollback(dep.id)}
                disabled={!!active}
                className="px-3 py-1 text-[11px] font-medium text-gray-600 dark:text-gray-300 border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-50 dark:hover:bg-gray-800 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
              >
                回退到 {dep.previous_version}
              </button>
            )}
          </div>
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

        <div className="flex flex-wrap items-center gap-2 mb-4">
          <button
            onClick={() => void runCheck()}
            disabled={checking}
            className="px-3 py-1.5 text-xs font-medium text-gray-700 dark:text-gray-200 border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-50 dark:hover:bg-gray-800 disabled:opacity-50 transition-colors"
          >
            {checking ? '检查中…' : '检查模型更新'}
          </button>
          {updates && !updates.ok && (
            <span className="text-xs text-gray-400">没有取到更新清单（离线不影响使用当前版本）</span>
          )}
          {updates?.ok && updates.updates.length === 0 && (
            <span className="text-xs text-gray-400">模型已是最新</span>
          )}
          {updates?.ok && updates.updates.length > 0 && (
            <span className="text-xs text-emerald-600 dark:text-emerald-400">
              {updates.updates.length} 个模型可升级
            </span>
          )}
          {upgradeMsg && <span className="text-xs text-red-500">{upgradeMsg}</span>}
        </div>

        <label className="flex items-start gap-2 mb-4 text-xs text-gray-600 dark:text-gray-300">
          <input
            type="checkbox"
            checked={autoUpdate}
            onChange={e => void toggleAutoUpdate(e.target.checked)}
            className="mt-0.5 accent-blue-600"
          />
          <span>
            后台自动升级模型
            <span className="text-gray-400">
              （默认关闭：只在上面提示"可升级"，不自动下载；打开后启动 30 秒后台静默升级，失败保持旧版本）
            </span>
          </span>
        </label>

        {note}

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
