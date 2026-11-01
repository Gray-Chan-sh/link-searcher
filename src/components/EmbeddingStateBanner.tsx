import { useI18n } from '../i18n'
import { LoadingSpinner } from '../icons'
import type { EmbeddingConsistency } from '../api/index'

interface Props {
  c: EmbeddingConsistency
  /** 重建/扫描进行中（按钮禁用） */
  busy: boolean
  onRebuild: () => void
  onClaim: () => void
  onRecheck: () => void
}

/**
 * 语义向量状态横幅（五态）。
 *
 * 只在**需要用户做点什么**时出现：
 *
 * | 状态 | 横幅 | 原因 |
 * |------|------|------|
 * | `current` | 无 | 正常，不打扰 |
 * | `empty` | 无 | 还没有向量，回填会自动做 |
 * | `stale` | 红 | 库里的向量属于**另一个**模型 → 语义检索已暂停 |
 * | `mixed` | 红 | 两个以上空间混在一起 → 语义检索已暂停 |
 * | `legacy` | 黄 | 来源未知（v14 之前写入）→ 不阻断，但可以认领 |
 *
 * `legacy` 用黄色而不是红色是刻意的：升级后的第一天必然是 legacy，把它标成
 * 故障会让所有人第一眼就以为坏了。它确实是"不知道"，那就只说不道。
 */
export default function EmbeddingStateBanner({ c, busy, onRebuild, onClaim, onRecheck }: Props) {
  const { t } = useI18n()
  const state = c.corpus_state
  if (state === 'current' || state === 'empty') return null

  const danger = state === 'stale' || state === 'mixed'
  const tone = danger
    ? {
        box: 'bg-red-50 dark:bg-red-900/20 border-red-200 dark:border-red-800',
        icon: 'text-red-500',
        title: 'text-red-800 dark:text-red-200',
        body: 'text-red-700 dark:text-red-300',
        btn: 'bg-red-600 hover:bg-red-700 text-white',
      }
    : {
        box: 'bg-amber-50 dark:bg-amber-900/20 border-amber-200 dark:border-amber-800',
        icon: 'text-amber-500',
        title: 'text-amber-800 dark:text-amber-200',
        body: 'text-amber-700 dark:text-amber-300',
        btn: 'bg-amber-600 hover:bg-amber-700 text-white',
      }

  const title = danger
    ? state === 'mixed'
      ? t('embed_state_mixed_title')
      : t('embed_state_stale_title')
    : t('embed_state_legacy_title')

  const body = danger
    ? state === 'mixed'
      ? t('embed_state_mixed_body', { n: c.fp_counts.filter(f => f.doc_rows + f.chunk_rows > 0).length })
      : t('embed_state_stale_body', { model: c.active_model })
    : t('embed_state_legacy_body', { rows: c.legacy_rows })

  const dims = c.expected_dim != null && c.stored_dims.length > 0 && !c.stored_dims.every(d => d === c.expected_dim)
    ? t('embed_state_dim_mismatch', { expected: c.expected_dim, stored: c.stored_dims.join('/') })
    : null

  return (
    <div className={`mb-6 p-4 border rounded-lg ${tone.box}`}>
      <div className="flex items-start gap-3">
        <span className={`text-lg leading-none mt-0.5 ${tone.icon}`}>⚠</span>
        <div className="flex-1 text-sm">
          <p className={`font-medium ${tone.title}`}>{title}</p>
          <p className={`mt-1 ${tone.body}`}>{body}</p>
          {dims && <p className={`mt-1 ${tone.body}`}>{dims}</p>}
          {c.fp_counts.length > 0 && (
            <p className={`mt-1 text-xs ${tone.body} opacity-80`}>
              {c.fp_counts
                .map(f => `${f.model_fp === '' ? t('embed_state_unknown_fp') : f.model_fp}: ${f.doc_rows + f.chunk_rows}`)
                .join(' · ')}
            </p>
          )}
          {(c.pending_docs > 0 || c.pending_chunks > 0) && (
            <p className={`mt-1 text-xs ${tone.body} opacity-80`}>
              {t('embed_state_pending', { docs: c.pending_docs, chunks: c.pending_chunks })}
            </p>
          )}
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <button
              onClick={onRebuild}
              disabled={busy}
              className={`flex items-center gap-2 px-3 py-1.5 text-sm font-medium rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${tone.btn}`}
            >
              {busy && <LoadingSpinner className="size-4" />}
              {t('embed_state_rebuild')}
            </button>
            {state === 'legacy' && (
              <button
                onClick={onClaim}
                disabled={busy}
                title={t('embed_state_claim_hint')}
                className="px-3 py-1.5 text-sm font-medium rounded-lg border border-amber-400 text-amber-700 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900/40 disabled:opacity-50 transition-colors"
              >
                {t('embed_state_claim')}
              </button>
            )}
            <button
              onClick={onRecheck}
              className="px-3 py-1.5 text-sm font-medium rounded-lg border border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors"
            >
              {t('embed_state_recheck')}
            </button>
          </div>
        </div>
      </div>
    </div>
  )
}
