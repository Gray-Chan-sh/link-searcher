/** 父路径吞并子路径（与后端 merge_scope_prefixes 语义一致）：
 *  - 空字符串吸收：若输入包含 ""，直接返回 [""]（空串 = 全库，是一切路径的前缀）
 *  - 父吞子：A 与 A/B 并存时保留 A（短前缀胜出）
 *  - 尾部斜杠归一化：比较前去除尾部 '/'
 *  - 过滤掉空字符串（吸收情况除外）
 */
export function mergeScopePrefixes(paths: string[]): string[] {
  const trimmed = paths.map(p => p.trim().replace(/\/+$/, ''))
  // 空字符串吸收
  if (trimmed.some(p => p === '')) return ['']
  // 去精确重复 + 父吞子
  const unique = [...new Set(trimmed.filter(Boolean))]
  return unique.filter((p, i) =>
    !unique.some((q, j) => j !== i && p.startsWith(q + '/'))
  )
}