// 恢复前目录映射的纯规则（无 React 依赖，便于单测）：
// 预检「命中率偏低」判定、路径归一化、重复目标与嵌套根判定。
// 这些规则与后端 `commands/backup.rs::validate_mappings` / `path_key` 保持一致。
import type { RemapPreview } from '../../api/backup'

/** 命中率低于此值即视为「内容可能不符」，需要用户显式确认。 */
export const LOW_HIT_RATIO = 0.5
/** 记录太少时命中率无统计意义，不触发警告。 */
export const MIN_TRACKED_FOR_WARNING = 20

export function isLowHit(p: RemapPreview): boolean {
  return p.tracked >= MIN_TRACKED_FOR_WARNING && p.found / p.tracked < LOW_HIT_RATIO
}

/** 路径归一化 key：统一分隔符、去尾部分隔符、整串小写（仅用于判重/判嵌套）。 */
export function normPath(p: string): string {
  const s = p.replace(/\\/g, '/').replace(/\/+$/, '')
  return (s === '' ? '/' : s).toLowerCase()
}

/** 是否存在「一个目标位于另一个之下」的嵌套（会重复索引）。 */
export function hasNestedRoots(paths: string[]): boolean {
  const keys = paths.map(normPath)
  return keys.some((a, i) => keys.some((b, j) => i !== j && b.startsWith(`${a}/`)))
}

/** 是否有两个目录被映射到同一个目标路径。 */
export function hasDuplicateTargets(paths: string[]): boolean {
  const keys = paths.map(normPath)
  return new Set(keys).size !== keys.length
}
