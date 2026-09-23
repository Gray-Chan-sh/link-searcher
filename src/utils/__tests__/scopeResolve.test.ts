import { describe, it, expect } from 'vitest'
import { pickScopeDir } from '../scopeResolve'

describe('pickScopeDir', () => {
  it('returns null for empty name or no matches', () => {
    expect(pickScopeDir([], '案件')).toBeNull()
    expect(pickScopeDir(['案件/一审'], '   ')).toBeNull()
  })

  it('prefers the exact directory match', () => {
    expect(pickScopeDir(['案件/一审', '案件/二审', '案件'], '案件')).toBe('案件')
  })

  it('matches nested directories by their last segment (case-insensitive)', () => {
    expect(pickScopeDir(['WJY 汪均益/行政诉讼/一审'], '一审')).toBe('WJY 汪均益/行政诉讼/一审')
    expect(pickScopeDir(['A/Case'], 'case')).toBe('A/Case')
  })

  it('prefers a directory over a same-named file', () => {
    expect(pickScopeDir(['案件.txt'], '案件')).toBe('案件.txt')
    expect(pickScopeDir(['docs/案件/正文', '案件.pdf'], '案件')).toBe('docs/案件/正文')
  })

  it('falls back to any directory before any file', () => {
    expect(pickScopeDir(['案件/判决书.pdf', 'x/案件.txt'], '案件')).toBe('案件/判决书.pdf')
  })
})
