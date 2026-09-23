import { describe, it, expect } from 'vitest'
import { parseScope } from '../scopeParser'

describe('parseScope', () => {
  it('S1: /ext + @文件 混用', () => {
    const r = parseScope('根据@财务/年度财务报告.md /ext:pdf 汇总收入')
    expect(r.scope.mention_files).toEqual(['财务/年度财务报告.md'])
    expect(r.scope.conditions).toEqual([{ kind: 'ext', value: 'pdf' }])
    expect(r.cleanText).toBe('根据 汇总收入')
    expect(r.scopeAction).toBeNull()
  })

  it('S2: 无命令时 conditions 空、原文保留', () => {
    const r = parseScope('为什么营收下降')
    expect(r.scope.conditions).toEqual([])
    expect(r.cleanText).toBe('为什么营收下降')
  })

  it('S3: /date', () => {
    const r = parseScope('/date:2025-01-01~2025-12-31 收据')
    expect(r.scope.conditions[0]).toEqual({ kind: 'date', value: '2025-01-01~2025-12-31' })
  })

  it('S4: /范围:全库 → clear', () => {
    expect(parseScope('/范围:全库 重新回答').scopeAction).toBe('clear')
  })

  it('S5: /范围:目录 → dir:', () => {
    expect(parseScope('/范围:财务 本季度').scopeAction).toBe('dir:财务')
  })

  it('S6: @目录 vs @文件 区分', () => {
    const r = parseScope('@财务 与 @年度报告.md 对比')
    expect(r.scope.mention_dirs).toEqual(['财务'])
    expect(r.scope.mention_files).toEqual(['年度报告.md'])
  })

  it('S7: @上轮/@第N轮 作为普通文本保留', () => {
    const r = parseScope('@上轮 那净利润呢')
    expect(r.scope.mention_files).toEqual([])
    expect(r.scope.mention_dirs).toEqual([])
    expect(r.cleanText).toBe('上轮 那净利润呢')
  })

  it('S8: /模糊 条件暂存原文', () => {
    const r = parseScope('/模糊:跟去年收购相关的 有哪些')
    expect(r.scope.conditions[0]).toEqual({ kind: 'fuzzy', value: '跟去年收购相关的' })
  })

  it('S9: URL 完整保留（M1 回归）', () => {
    const r = parseScope('帮我总结 https://example.com/report.pdf 的内容')
    expect(r.cleanText).toBe('帮我总结 https://example.com/report.pdf 的内容')
    expect(r.scope.conditions).toEqual([])
  })

  it('S10: @@ 双符号前缀不污染（M2 回归）', () => {
    const r = parseScope('对比 @@财务 和 健康')
    expect(r.scope.mention_dirs).toEqual(['财务'])
    expect(r.cleanText).toBe('对比 和 健康')
  })

  it('S11: 句尾孤立 @ 不报错、不产生 mention', () => {
    const r = parseScope('问题 @')
    expect(r.scope.mention_files).toEqual([])
    expect(r.scope.mention_dirs).toEqual([])
    expect(r.cleanText).toBe('问题')
  })

  it('S12: 含空格路径完整提取（M3 回归）', () => {
    const r = parseScope('@案件/WC 万城/诉讼案件/音字转换文字记录（20260326）.pdf ，第三人庄建军是否说过不参与公司的经营管理。')
    expect(r.scope.mention_files).toEqual(['案件/WC 万城/诉讼案件/音字转换文字记录（20260326）.pdf'])
    expect(r.cleanText).toBe('，第三人庄建军是否说过不参与公司的经营管理。')
  })

  it('S13: 目录路径不含空格保持原有行为', () => {
    const r = parseScope('@财务 本季度收入')
    expect(r.scope.mention_dirs).toEqual(['财务'])
    expect(r.cleanText).toBe('本季度收入')
  })

  it('S14: /范围 命令 → scopeAction（打字预览依赖）', () => {
    const r = parseScope('/范围:案件 总结')
    expect(r.scopeAction).toBe('dir:案件')
    expect(r.cleanText).toBe('总结')
  })

  it('S15: /范围:全库 → clear', () => {
    const r = parseScope('/范围:全库 看看')
    expect(r.scopeAction).toBe('clear')
    expect(r.cleanText).toBe('看看')
  })
})
