import { useState } from 'react'
import { useI18n } from '../../i18n'
import { LoadingSpinner } from '../../icons'
import { openDirectory } from '../../utils/platform'
import type { DirActionKind, DirMapping, RemapPreview, RestorePlan } from '../../api/backup'
import { hasNestedRoots, isLowHit } from './restoreMapping'

interface RestoreMappingDialogProps {
  plan: RestorePlan
  onPreview: (dirId: string, newPath: string) => Promise<RemapPreview>
  onApply: (mappings: DirMapping[], acknowledgeRisks: boolean) => void
  onCancel: () => void
}

type Row = { action: DirActionKind | ''; path: string }
type PreviewState = { kind: 'loading' } | { kind: 'ok'; data: RemapPreview } | { kind: 'error'; message: string }

/** 目前只实现前三种落位；`disable`（暂时停用但保留数据）在后续版本。 */
const ACTIONS: DirActionKind[] = ['keep', 'remap', 'remove']

export function RestoreMappingDialog({ plan, onPreview, onApply, onCancel }: RestoreMappingDialogProps) {
  const { t } = useI18n()
  const verifyOk = plan.verify.mismatches.length === 0 && plan.verify.missing.length === 0
  const [rows, setRows] = useState<Record<string, Row>>(() => {
    const init: Record<string, Row> = {}
    for (const d of plan.dirs) init[d.id] = { action: '', path: '' }
    return init
  })
  const [previews, setPreviews] = useState<Record<string, PreviewState>>({})
  const [acked, setAcked] = useState(false)
  const [busy, setBusy] = useState(false)

  const setRow = (id: string, patch: Partial<Row>) => setRows(prev => ({ ...prev, [id]: { ...prev[id], ...patch } }))

  const runPreview = async (id: string, path: string) => {
    setPreviews(prev => ({ ...prev, [id]: { kind: 'loading' } }))
    try {
      const data = await onPreview(id, path)
      setPreviews(prev => ({ ...prev, [id]: { kind: 'ok', data } }))
    } catch (e) {
      setPreviews(prev => ({ ...prev, [id]: { kind: 'error', message: e instanceof Error ? e.message : String(e) } }))
    }
  }

  const pickDir = async (id: string) => {
    const picked = await openDirectory()
    if (!picked) return
    setRow(id, { action: 'remap', path: picked })
    await runPreview(id, picked)
  }

  /// 「保留原路径」也要预检：备份里的原路径在本机**存在但内容不符**时，
  /// 正是 R1 要防的危险场景 —— 命中率会很低，据此触发确认。
  const chooseAction = (d: BackupDirInfo, action: DirActionKind) => {
    if (action === 'remap') {
      void pickDir(d.id)
      return
    }
    if (action === 'keep') {
      setRow(d.id, { action, path: d.originalPath })
      if (d.originalPath.trim() !== '' && d.existsLocally) void runPreview(d.id, d.originalPath)
      return
    }
    setRow(d.id, { action, path: '' })
  }

  /** 某行是否处于「有目标路径、可/已预检」的状态（remap 与 keep 都要看命中率）。 */
  const previewable = (r?: Row) => (r?.action === 'remap' || r?.action === 'keep') && r.path.trim() !== ''

  const allDecided = plan.dirs.every(d => rows[d.id]?.action !== '')
  const remapComplete = plan.dirs.every(d => {
    const r = rows[d.id]
    return r?.action !== 'remap' || r.path.trim() !== ''
  })
  const hasLowHit = plan.dirs.some(d => {
    if (!previewable(rows[d.id])) return false
    const p = previews[d.id]
    return p?.kind === 'ok' && isLowHit(p.data)
  })
  // 归一化后判嵌套（与后端 `validate_mappings` 同一规则）：
  const hasNesting = hasNestedRoots(
    plan.dirs
      .filter(d => rows[d.id]?.action === 'remap' && rows[d.id].path.trim() !== '')
      .map(d => rows[d.id].path),
  )
  const hasRisk = hasLowHit || hasNesting
  const canApply = allDecided && remapComplete && (!hasRisk || acked) && !busy

  const handleApply = () => {
    if (!canApply) return
    const mappings: DirMapping[] = plan.dirs.map(d => {
      const r = rows[d.id]
      return {
        dirId: d.id,
        action: r.action as DirActionKind,
        newPath: r.action === 'remap' ? r.path : null,
      }
    })
    setBusy(true)
    onApply(mappings, hasRisk && acked)
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
      <div className="w-full max-w-3xl max-h-[85vh] flex flex-col bg-white dark:bg-gray-900 rounded-xl shadow-xl border border-gray-200 dark:border-gray-700">
        <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-700">
          <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('backup_mapping_title')}</h3>
          <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">{t('backup_mapping_desc')}</p>
          {!verifyOk && (
            <p className="mt-1 text-xs text-amber-600 dark:text-amber-400">
              {t('backup_verify_warn', { n: String(plan.verify.mismatches.length + plan.verify.missing.length) })}
            </p>
          )}
        </div>

        <div className="flex-1 overflow-y-auto px-4 py-3 space-y-3">
          {plan.dirs.length === 0 && (
            <p className="text-sm text-gray-500 dark:text-gray-400">{t('backup_mapping_no_dirs')}</p>
          )}
          {plan.dirs.map(d => {
            const r = rows[d.id]
            const p = previews[d.id]
            return (
              <div key={d.id} className="p-3 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/40">
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <p className="text-sm font-mono text-gray-900 dark:text-gray-100 truncate">{d.originalPath}</p>
                    <p className="text-xs text-gray-500 dark:text-gray-400">
                      {d.alias ? `${d.alias} · ` : ''}{t('backup_dead_dir_files', { n: String(d.fileCount) })}
                      {d.existsLocally ? ` · ${t('backup_mapping_exists_local')}` : ''}
                    </p>
                  </div>
                  <div className="shrink-0 flex items-center gap-1">
                    {ACTIONS.map(a => (
                      <button
                        key={a}
                        onClick={() => chooseAction(d, a)}
                        className={`px-2 py-1 text-xs font-medium rounded transition-colors ${
                          r?.action === a
                            ? 'bg-blue-600 text-white'
                            : 'bg-white dark:bg-gray-800 text-gray-600 dark:text-gray-300 border border-gray-200 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700'
                        }`}
                      >
                        {t(`backup_mapping_act_${a}`)}
                      </button>
                    ))}
                  </div>
                </div>

                {previewable(r) && (
                  <div className="mt-2 space-y-1">
                    <div className="flex items-center gap-2">
                      <span className="flex-1 min-w-0 text-xs font-mono text-gray-700 dark:text-gray-200 truncate">
                        {r.path || t('backup_mapping_no_path')}
                      </span>
                      {r.action === 'remap' && (
                        <button
                          onClick={() => void pickDir(d.id)}
                          className="shrink-0 px-2 py-1 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
                        >
                          {t('backup_mapping_pick')}
                        </button>
                      )}
                    </div>
                    {p?.kind === 'loading' && (
                      <p className="flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400">
                        <LoadingSpinner className="size-3" />{t('backup_mapping_previewing')}
                      </p>
                    )}
                    {p?.kind === 'error' && <p className="text-xs text-red-500">{p.message}</p>}
                    {p?.kind === 'ok' && (
                      <p className={`text-xs ${isLowHit(p.data) ? 'text-amber-600 dark:text-amber-400' : 'text-gray-500 dark:text-gray-400'}`}>
                        {t('backup_mapping_preview', {
                          found: String(p.data.found),
                          tracked: String(p.data.tracked),
                          size: String(p.data.sizeMatch),
                        })}
                        {isLowHit(p.data) ? ` — ${t('backup_mapping_low_hit')}` : ''}
                      </p>
                    )}
                  </div>
                )}

                {r?.action === 'remove' && (
                  <p className="mt-2 text-xs text-red-500">{t('backup_mapping_remove_hint')}</p>
                )}
              </div>
            )
          })}
        </div>

        <div className="px-4 py-3 border-t border-gray-200 dark:border-gray-700 space-y-2">
          {hasRisk && (
            <div className="space-y-1">
              {hasNesting && (
                <p className="text-xs text-amber-600 dark:text-amber-400">{t('backup_mapping_nested')}</p>
              )}
              <label className="flex items-start gap-2 text-xs text-amber-600 dark:text-amber-400">
                <input type="checkbox" checked={acked} onChange={e => setAcked(e.target.checked)} className="mt-0.5" />
                <span>{t('backup_mapping_ack')}</span>
              </label>
            </div>
          )}
          <div className="flex items-center justify-end gap-2">
            <button
              onClick={onCancel}
              disabled={busy}
              className="px-3 py-1.5 text-xs font-medium text-gray-600 dark:text-gray-300 bg-gray-100 dark:bg-gray-800 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-700 disabled:opacity-50 transition-colors"
            >
              {t('backup_mapping_cancel')}
            </button>
            <button
              onClick={handleApply}
              disabled={!canApply}
              className="px-3 py-1.5 text-xs font-medium text-white bg-amber-600 hover:bg-amber-700 rounded-lg disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {busy ? t('backup_mapping_applying') : t('backup_mapping_start')}
            </button>
          </div>
        </div>
      </div>
    </div>
  )
}
