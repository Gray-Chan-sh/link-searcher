import { describe, it, expect } from 'vitest'
import type { DirEntry } from '../../api/files.ts'
import {
  breadcrumbs,
  canGoUp,
  cleanPath,
  entryExt,
  entryStatus,
  entryTypeLabel,
  filterEntries,
  isSamePath,
  isWithinRoot,
  joinPath,
  parentPath,
  resolveRelPath,
  rootFor,
  sortEntries,
  totalSize,
} from '../folderViewUtils.ts'

function entry(partial: Partial<DirEntry> & { name: string }): DirEntry {
  return {
    name: partial.name,
    path: partial.path ?? `D:/docs/${partial.name}`,
    is_dir: partial.is_dir ?? false,
    is_supported: partial.is_supported ?? true,
    file_size: partial.file_size ?? 0,
    mtime: partial.mtime ?? 0,
    indexed: partial.indexed ?? 0,
    error_msg: partial.error_msg ?? null,
    file_id: partial.file_id ?? null,
    status: partial.status ?? null,
    quality_score: partial.quality_score ?? null,
    indexed_at: partial.indexed_at ?? null,
  }
}

describe('path helpers', () => {
  it('normalizes separators and trailing slashes', () => {
    expect(cleanPath('D:\\docs\\a\\')).toBe('D:/docs/a')
    expect(cleanPath('D:/')).toBe('D:/')
  })

  it('computes parent paths (drive root stays put)', () => {
    expect(parentPath('D:/docs/a')).toBe('D:/docs')
    expect(parentPath('D:/docs')).toBe('D:/')
    expect(parentPath('D:/')).toBe('D:/')
    expect(parentPath('/home/gray')).toBe('/home')
  })

  it('joins without doubling slashes', () => {
    expect(joinPath('D:/docs/', 'a.docx')).toBe('D:/docs/a.docx')
    expect(joinPath('D:/docs', 'sub')).toBe('D:/docs/sub')
  })

  it('compares Windows paths case-insensitively', () => {
    expect(isSamePath('D:/Docs/A.docx', 'd:/docs/a.docx')).toBe(true)
  })

  it('builds breadcrumbs from the root label down', () => {
    const crumbs = breadcrumbs('D:/docs', '资料库', 'D:/docs/案件/CH案')
    expect(crumbs.map(c => c.label)).toEqual(['资料库', '案件', 'CH案'])
    expect(crumbs[2]!.path).toBe('D:/docs/案件/CH案')
  })

  it('falls back to the root when the current path is outside it', () => {
    const crumbs = breadcrumbs('D:/docs', '资料库', 'D:/other/x')
    expect(crumbs).toHaveLength(1)
  })

  it('resolves a DB-relative deep link against registered roots', () => {
    expect(resolveRelPath('案件/a.docx', ['D:/docs', 'E:/work'])).toEqual({
      rootPath: 'D:/docs',
      absPath: 'D:/docs/案件/a.docx',
    })
  })

  it('rejects traversal and empty deep links', () => {
    expect(resolveRelPath('../etc/passwd', ['D:/docs'])).toBeNull()
    expect(resolveRelPath('', ['D:/docs'])).toBeNull()
  })

  it('detects containment and the most specific root', () => {
    expect(isWithinRoot('D:/docs/a.docx', 'D:/docs')).toBe(true)
    expect(isWithinRoot('D:/docs2/a.docx', 'D:/docs')).toBe(false)
    expect(isWithinRoot('d:/DOCS/a.docx', 'D:/docs')).toBe(true)
    expect(rootFor('D:/docs/sub/a.docx', ['D:/docs', 'D:/docs/sub'])).toBe('D:/docs/sub')
    expect(rootFor('E:/other/a.docx', ['D:/docs'])).toBeNull()
  })

  it('never allows going above the library root', () => {
    // 在根上：不能再往上（这正是会把 D:\ 暴露出来的入口）
    expect(canGoUp('D:/docs', 'D:/docs')).toBe(false)
    expect(canGoUp('D:/', 'D:/docs')).toBe(false)
    // 根之内：可以
    expect(canGoUp('D:/docs/sub', 'D:/docs')).toBe(true)
    // 盘符根作为资料库时，上一级同样止步于根
    expect(canGoUp('D:/', 'D:/')).toBe(false)
  })
})

describe('sortEntries', () => {
  const dirs = [entry({ name: 'zeta', is_dir: true }), entry({ name: 'alpha' }), entry({ name: 'beta' })]

  it('always puts directories first, whatever the order', () => {
    const desc = sortEntries(dirs, 'name', 'desc')
    expect(desc.map(e => e.name)).toEqual(['zeta', 'beta', 'alpha'])
    const asc = sortEntries(dirs, 'name', 'asc')
    expect(asc.map(e => e.name)).toEqual(['zeta', 'alpha', 'beta'])
  })

  it('sorts by mtime and size', () => {
    const files = [
      entry({ name: 'old', mtime: 1, file_size: 10 }),
      entry({ name: 'new', mtime: 5, file_size: 1 }),
    ]
    expect(sortEntries(files, 'mtime', 'desc').map(e => e.name)).toEqual(['new', 'old'])
    expect(sortEntries(files, 'size', 'desc').map(e => e.name)).toEqual(['old', 'new'])
  })

  it('sorts by status with failures first', () => {
    const files = [
      entry({ name: 'ok', indexed: 1 }),
      entry({ name: 'bad', indexed: 2 }),
      entry({ name: 'todo', indexed: 0 }),
    ]
    expect(sortEntries(files, 'status', 'asc').map(e => e.name)).toEqual(['bad', 'ok', 'todo'])
  })

  it('sorts by last indexed time, never-indexed last', () => {
    const files = [
      entry({ name: 'old', indexed_at: 1_700_000_000 }),
      entry({ name: 'new', indexed_at: 1_800_000_000 }),
      entry({ name: 'none', indexed_at: null }),
    ]
    expect(sortEntries(files, 'indexed_at', 'desc').map(e => e.name)).toEqual(['new', 'old', 'none'])
  })

  it('sorts by quality, unscored last', () => {
    const files = [
      entry({ name: 'none', quality_score: null }),
      entry({ name: 'high', quality_score: 0.95 }),
      entry({ name: 'low', quality_score: 0.4 }),
    ]
    expect(sortEntries(files, 'quality', 'desc').map(e => e.name)).toEqual(['high', 'low', 'none'])
  })
})

describe('filterEntries', () => {
  const entries = [entry({ name: '起诉状.docx', path: 'D:/案件/起诉状.docx' }), entry({ name: '证据.pdf' })]
  it('matches name or full path, case-insensitively', () => {
    expect(filterEntries(entries, '起诉')).toHaveLength(1)
    expect(filterEntries(entries, 'D:/案件')).toHaveLength(1)
    expect(filterEntries(entries, 'PDF')).toHaveLength(1)
  })
  it('returns everything for an empty filter', () => {
    expect(filterEntries(entries, '  ')).toHaveLength(2)
  })
})

describe('entry metadata', () => {
  it('derives type labels and extensions', () => {
    expect(entryTypeLabel(entry({ name: 'x', is_dir: true }))).toBe('folder')
    expect(entryTypeLabel(entry({ name: 'a.DOCX' }))).toBe('docx')
    expect(entryTypeLabel(entry({ name: 'a.bin', is_supported: false }))).toBe('unsupported')
    expect(entryExt(entry({ name: 'a.docx' }))).toBe('docx')
    expect(entryExt(entry({ name: 'noext' }))).toBe('')
  })

  it('maps indexed codes to statuses', () => {
    expect(entryStatus(entry({ name: 'd', is_dir: true }))).toBe('folder')
    expect(entryStatus(entry({ name: 'x', is_supported: false }))).toBe('unsupported')
    expect(entryStatus(entry({ name: 'x', indexed: 1 }))).toBe('indexed')
    expect(entryStatus(entry({ name: 'x', indexed: 3 }))).toBe('extracting')
    expect(entryStatus(entry({ name: 'x', indexed: 2 }))).toBe('failed')
    expect(entryStatus(entry({ name: 'x', indexed: 0 }))).toBe('pending')
  })

  it('sums file sizes and ignores directories', () => {
    expect(totalSize([
      entry({ name: 'a', file_size: 10 }),
      entry({ name: 'b', file_size: 5 }),
      entry({ name: 'd', is_dir: true, file_size: 999 }),
    ])).toBe(15)
  })
})
