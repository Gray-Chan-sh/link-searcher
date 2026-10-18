import { describe, it, expect } from 'vitest'
import { isLowHit, normPath, hasNestedRoots, hasDuplicateTargets, MIN_TRACKED_FOR_WARNING } from '../restoreMapping'

describe('isLowHit', () => {
  it('记录太少不告警', () => {
    expect(isLowHit({ tracked: 5, found: 0, sizeMatch: 0, sampleMissing: [] })).toBe(false)
  })
  it('命中率低于阈值才告警', () => {
    const n = MIN_TRACKED_FOR_WARNING
    expect(isLowHit({ tracked: n, found: n / 2 - 1, sizeMatch: 0, sampleMissing: [] })).toBe(true)
    expect(isLowHit({ tracked: n, found: n / 2, sizeMatch: 0, sampleMissing: [] })).toBe(false)
  })
  it('tracked=0 视为不告警', () => {
    expect(isLowHit({ tracked: 0, found: 0, sizeMatch: 0, sampleMissing: [] })).toBe(false)
  })
})

describe('normPath', () => {
  it('统一分隔符、去尾部分隔符并小写', () => {
    expect(normPath('C:\\Docs\\Sub\\')).toBe('c:/docs/sub')
    expect(normPath('/mnt/Docs/')).toBe('/mnt/docs')
  })
  it('根路径保留为 /', () => {
    expect(normPath('/')).toBe('/')
    expect(normPath('////')).toBe('/')
  })
})

describe('hasNestedRoots', () => {
  it('识别一个目录位于另一个之下（跨分隔符）', () => {
    expect(hasNestedRoots(['C:\\Docs', 'C:\\Docs\\Sub'])).toBe(true)
    expect(hasNestedRoots(['C:\\Docs', 'D:\\Docs'])).toBe(false)
  })
  it('仅前缀相同但非父目录不算嵌套', () => {
    expect(hasNestedRoots(['C:\\Docs', 'C:\\Docs2'])).toBe(false)
  })
  it('单目录不嵌套', () => {
    expect(hasNestedRoots(['C:\\Docs'])).toBe(false)
  })
})

describe('hasDuplicateTargets', () => {
  it('归一化后相同即重复', () => {
    expect(hasDuplicateTargets(['C:\\Docs\\', 'c:/docs'])).toBe(true)
    expect(hasDuplicateTargets(['C:\\Docs', 'C:\\Other'])).toBe(false)
  })
})
