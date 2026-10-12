// 回归：`remove_dir` 的返回值归一化。
//
// 背景（线上事故）：Tauri 模式下 `client.invoke` 返回**原始命令结果**，
// `transform` 只作用于 HTTP 传输。命令改成返回 `RemoveDirReport` 后，
// 前端把 `{message, stopped_scan, ...}` 对象当成字符串渲染，
// 直接触发 React error #31（"Objects are not valid as a React child"）。
//
// 所以 `removeDir` 必须把两种传输的形态都归一化成 string。

import { describe, it, expect, vi } from 'vitest'

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }))
vi.mock('../client', () => ({ invoke: invokeMock }))

import { removeDir } from '../dirs'

describe('removeDir 归一化返回值', () => {
  it('Tauri 形态（report 对象）→ 取出 message', async () => {
    invokeMock.mockResolvedValueOnce({
      message: '已移除目录并清理数据（清理残留目录 2 个）；已继续索引其它目录',
      stopped_scan: true,
      swept_orphan_dirs: 2,
      resumed_scan: true,
    })
    await expect(removeDir('d1', true)).resolves.toBe(
      '已移除目录并清理数据（清理残留目录 2 个）；已继续索引其它目录',
    )
    expect(invokeMock).toHaveBeenCalledWith('remove_dir', { id: 'd1', purge: true })
  })

  it('HTTP 形态（transform 已取出 message 字符串）→ 原样返回', async () => {
    invokeMock.mockResolvedValueOnce('已移除')
    await expect(removeDir('d1')).resolves.toBe('已移除')
  })

  it('任何情况下都不会返回非字符串（React #31 防线）', async () => {
    invokeMock.mockResolvedValueOnce(undefined)
    await expect(removeDir('d1')).resolves.toBe('')
    invokeMock.mockResolvedValueOnce({ message: undefined })
    await expect(removeDir('d1')).resolves.toBe('')
  })
})
