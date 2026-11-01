import { useI18n } from '../../i18n'
import type { EmbeddingSwitchPreview, SwitchVerdict } from '../../api/config'

export interface SwitchPrompt {
  /** 目标模型 id（`provider:model` 或 `local:<name>`） */
  modelId: string
  /** 给人看的模型名，不带 provider uuid */
  modelLabel: string
  preview: EmbeddingSwitchPreview
}

interface Props {
  prompt: SwitchPrompt
  /** 需要重建的内容/块数在预览里，这里只用它展示代价 */
  busy: boolean
  onCancel: () => void
  onSwitchOnly: () => void
  onRebuild: () => void
}

/**
 * 切换嵌入模型时的三选一对话框。
 *
 * ## 为什么不是二选一
 *
 * "要不要重建"的两个答案都会让人后悔：不重建 → 语义检索静默用错空间；重建 →
 * 十几万条向量重算几小时。中间必须有一条"我知道我在干什么，先切过去，语义检索
 * 暂停"的路（例如只是想先把模型配好、明天再重建）。所以是**取消 / 切换并重建 /
 * 仅切换**。
 *
 * ## 措辞由"怎么知道的"决定，不由严重程度决定
 *
 * `different_local`：两边都是本地模型，权重目录哈希直接可比 → 可以说"不一致"。
 * `unknown`：含远程网关，同一个 URL 背后的权重可能已经换了 → 只能说"无法确认"。
 * `legacy_unknown`：旧版本写入，压根没有来源记录 → 只能说"来源未知"。
 *
 * 把三种情况都写成"不一致"是**编造确定性**：用户会以为系统验证过，等哪天结果
 * 不对时只会怀疑检索本身。诚实说明反而能让用户做出正确选择。
 */
export default function EmbeddingSwitchDialog({
  prompt,
  busy,
  onCancel,
  onSwitchOnly,
  onRebuild,
}: Props) {
  const { t } = useI18n()
  const { preview, modelLabel } = prompt

  const body = bodyForVerdict(preview.verdict, modelLabel, preview.pending_docs, preview.pending_chunks, t)

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
      <div className="w-full max-w-lg rounded-lg bg-white dark:bg-neutral-900 shadow-xl border border-neutral-200 dark:border-neutral-700">
        <div className="px-4 pt-4">
          <h3 className="text-sm font-semibold text-neutral-900 dark:text-neutral-100">
            {t('embed_switch_title')}
          </h3>
          <p className="mt-2 text-xs leading-relaxed text-neutral-600 dark:text-neutral-300">{body}</p>
          <p className="mt-2 text-xs leading-relaxed text-neutral-500 dark:text-neutral-400">
            {t('embed_switch_explain')}
          </p>
        </div>
        <div className="flex items-center justify-end gap-2 px-4 py-3">
          <button
            type="button"
            disabled={busy}
            onClick={onCancel}
            className="px-3 py-1 text-xs rounded border border-neutral-300 dark:border-neutral-600 text-neutral-700 dark:text-neutral-300 hover:bg-neutral-100 dark:hover:bg-neutral-800 disabled:opacity-50"
          >
            {t('embed_switch_cancel')}
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={onSwitchOnly}
            className="px-3 py-1 text-xs rounded border border-amber-400 text-amber-700 dark:text-amber-400 hover:bg-amber-50 dark:hover:bg-amber-950/40 disabled:opacity-50"
          >
            {/* 只有在"确实会暂停语义检索"（stale/mixed）时才敢这么承诺。
                legacy 状态下语义检索本来就能跑，写"暂停"就是在骗用户。 */}
            {preview.rebuild_required ? t('embed_switch_only') : t('embed_switch_only_plain')}
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={onRebuild}
            className="px-3 py-1 text-xs font-medium rounded bg-purple-600 text-white hover:bg-purple-700 disabled:opacity-50"
          >
            {busy ? t('embed_switch_busy') : t('embed_switch_rebuild')}
          </button>
        </div>
      </div>
    </div>
  )
}

function bodyForVerdict(
  verdict: SwitchVerdict,
  model: string,
  docs: number,
  chunks: number,
  t: (key: string, params?: Record<string, string | number>) => string,
): string {
  const cost = t('embed_switch_cost', { docs, chunks })
  switch (verdict) {
    case 'different_local':
      return `${t('embed_switch_body_local', { model })} ${cost}`
    case 'legacy_unknown':
      return `${t('embed_switch_body_legacy', { model, rows: docs + chunks })} ${cost}`
    case 'unknown':
    default:
      return `${t('embed_switch_body_unknown', { model })} ${cost}`
  }
}
