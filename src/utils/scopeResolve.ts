// /范围:<名称> 解析：把用户输入的目录名解析为可用的检索范围路径。
//
// 目录树是懒加载的（只含监控根 + 已展开层），因此不能只查内存中的树；
// 这里接收 `search_tree_prune` 返回的全树匹配前缀，挑选最合适的目录路径。
import { isFileLike } from './scopeParser'

const lastSegment = (path: string): string => path.split('/').pop() ?? ''

/**
 * 从全树匹配前缀里挑出代表 `dirName` 的范围路径。
 *
 * 优先级：末段精确匹配的目录 > 末段精确匹配的任意项 > 任意目录 > 首个匹配。
 * 全部不匹配时返回 null（调用方应视为无法解析，不做任何范围变更）。
 */
export function pickScopeDir(matches: string[], dirName: string): string | null {
  const name = dirName.trim().toLowerCase()
  if (!name) return null

  const exact = matches.filter(p => lastSegment(p).toLowerCase() === name)
  const exactDir = exact.find(p => !isFileLike(p))
  if (exactDir) return exactDir
  if (exact.length > 0) return exact[0]!

  const anyDir = matches.find(p => !isFileLike(p))
  if (anyDir) return anyDir
  return matches[0] ?? null
}
