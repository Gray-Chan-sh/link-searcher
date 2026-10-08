// Web 模式下「命令缺 HTTP 映射」的行为测试。
//
// 背景：`client.ts` 过去对没有 MAPPINGS 的命令静默 `return []`，把
// 「Web 端未实现 / 映射缺失」伪装成「空数据」——页面不报错、只是空白
// （依赖中心整块消失就踩过这个坑）。现在改为显式抛错，这里守住该行为，
// 避免有人图省事又把它改回静默兜底。

import { describe, it, expect, vi } from 'vitest'
import { invoke } from '../client'

// 最小浏览器桩：`isTauri()` 需要 window 存在且不含 `__TAURI_INTERNALS__`，
// 才会走 HTTP 分支。
;(globalThis as unknown as { window: unknown }).window = {
  location: { origin: 'https://ls.test' },
}

describe('Web 模式缺映射', () => {
  it('抛错而非静默返回 []', async () => {
    await expect(invoke('definitely_not_mapped')).rejects.toThrow(/No HTTP mapping/)
  })

  it('在 fetch 之前就短路，不发任何请求', async () => {
    const fetchSpy = vi.fn()
    ;(globalThis as unknown as { fetch: unknown }).fetch = fetchSpy
    await expect(invoke('definitely_not_mapped')).rejects.toThrow()
    expect(fetchSpy).not.toHaveBeenCalled()
  })
})
