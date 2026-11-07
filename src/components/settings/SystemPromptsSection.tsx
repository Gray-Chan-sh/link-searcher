import { useEffect, useState } from 'react'
import { useI18n } from '../../i18n'
import { Section } from './SettingsFields'
import { getSystemPrompts, type PromptTemplate } from '../../api/settings'

/** prompt id → i18n key。后端 `get_system_prompts` 的 id 必须在这里有对应文案。 */
const LABEL_KEYS: Record<string, string> = {
  query_rewrite: 'prompt_query_rewrite',
  rewrite_retry: 'prompt_rewrite_retry',
  gap_check: 'prompt_gap_check',
  answer: 'prompt_answer',
}

/**
 * 只读展示**实际生效**的系统提示词。
 *
 * 提示词硬编码在 Rust 里，用户既看不到也改不了；出了问题（例如"答得怪"）很难判断
 * 是"指令让它理解错"还是"材料没给对"。这里展示的就是后端源码里那一份（材料每轮
 * 不同，见「推理过程 → 提示词快照」），只读、不参与任何判断。
 */
export function SystemPromptsSection() {
  const { t } = useI18n()
  const [items, setItems] = useState<PromptTemplate[]>([])

  useEffect(() => {
    getSystemPrompts().then(setItems).catch(() => setItems([]))
  }, [])

  if (!items.length) return null

  return (
    <Section title={t('system_prompts')}>
      <p className="text-xs leading-relaxed text-gray-500 dark:text-gray-400 mb-3">
        {t('system_prompts_hint')}
      </p>
      <div className="space-y-2">
        {items.map(item => (
          <details key={item.id} className="border border-gray-200 dark:border-gray-700 rounded-lg">
            <summary className="cursor-pointer px-3 py-2 text-sm font-medium text-gray-700 dark:text-gray-200">
              {t(LABEL_KEYS[item.id] ?? item.id)}
            </summary>
            <pre className="px-3 pb-3 max-h-80 overflow-auto whitespace-pre-wrap break-words text-[11px] leading-relaxed text-gray-600 dark:text-gray-300">
              {item.text}
            </pre>
          </details>
        ))}
      </div>
    </Section>
  )
}
