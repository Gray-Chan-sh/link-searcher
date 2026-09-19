import { describe, it, expect } from 'vitest'
import { basename, dirname, truncateForDisplay, disambiguate, displayWidth } from '../materialLabel.ts'

describe('basename', () => {
  it('posix path', () => expect(basename('a/b/c.pdf')).toBe('c.pdf'))
  it('windows path', () => expect(basename('a\\b\\c.pdf')).toBe('c.pdf'))
  it('bare name', () => expect(basename('c.pdf')).toBe('c.pdf'))
  it('empty', () => expect(basename('')).toBe(''))
  it('trailing slash', () => expect(basename('a/b/')).toBe('b'))
})

describe('dirname', () => {
  it('posix path', () => expect(dirname('a/b/c.pdf')).toBe('a/b'))
  it('windows path', () => expect(dirname('a\\b\\c.pdf')).toBe('a\\b'))
  it('bare name', () => expect(dirname('c.pdf')).toBe(''))
  it('empty', () => expect(dirname('')).toBe(''))
})

describe('truncateForDisplay', () => {
  it('short ascii unchanged', () => expect(truncateForDisplay('a.pdf')).toBe('a.pdf'))

  it('long cjk keeps extension within width', () => {
    const out = truncateForDisplay('不动产登记资料查询申请书.pdf', 14)
    expect(displayWidth(out)).toBeLessThanOrEqual(14)
    expect(out.endsWith('.pdf')).toBe(true)
    expect(out).toContain('…')
  })

  it('name without extension', () => {
    const out = truncateForDisplay('这是一个非常非常长的文件名', 10)
    expect(displayWidth(out)).toBeLessThanOrEqual(10)
    expect(out).toContain('…')
  })
})

describe('disambiguate', () => {
  it('colliding basenames get distinct labels', () => {
    const paths = [
      '案件/WJY 汪均益/更正登记/委托书.docx',
      '案件/WJY 汪均益/更正登记/更正/委托书.docx',
      '案件/WJY 汪均益/更正登记/查阅/委托书.docx',
    ]
    const map = disambiguate(paths)
    const labels = paths.map(p => map.get(p)!)
    expect(new Set(labels).size).toBe(3)
    for (const l of labels) expect(l).toContain('委托书.docx')
  })

  it('unique basename keeps basename', () => {
    const map = disambiguate(['a/b/合同.pdf'])
    expect(map.get('a/b/合同.pdf')).toBe('合同.pdf')
  })

  it('duplicate paths dedupe', () => {
    const map = disambiguate(['a/x.docx', 'a/x.docx'])
    expect(map.size).toBe(1)
    expect(map.get('a/x.docx')).toBe('x.docx')
  })
})
