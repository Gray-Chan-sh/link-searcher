import { describe, it, expect } from 'vitest'
import { mergeScopePrefixes } from '../scopeMerge.ts'

describe('mergeScopePrefixes', () => {
  it('parent swallows child: ["A", "A/B"] → ["A"]', () => {
    expect(mergeScopePrefixes(['A', 'A/B'])).toEqual(['A'])
  })

  it('cross-root preservation: ["A", "B/C"] → ["A", "B/C"]', () => {
    expect(mergeScopePrefixes(['A', 'B/C'])).toEqual(['A', 'B/C'])
  })

  it('nested dedup to shortest: ["X/A/B", "X/A/B/C"] → ["X/A/B"]', () => {
    expect(mergeScopePrefixes(['X/A/B', 'X/A/B/C'])).toEqual(['X/A/B'])
  })

  it('empty input: [] → []', () => {
    expect(mergeScopePrefixes([])).toEqual([])
  })

  it('trailing slash normalization: ["A/", "A/B"] → ["A"]', () => {
    expect(mergeScopePrefixes(['A/', 'A/B'])).toEqual(['A'])
  })

  it('empty-string absorption: ["", "A/B"] → [""]', () => {
    expect(mergeScopePrefixes(['', 'A/B'])).toEqual([''])
  })

  it('empty-string absorption multiple: ["", "A", "B"] → [""]', () => {
    expect(mergeScopePrefixes(['', 'A', 'B'])).toEqual([''])
  })

  it('no parent relation: ["A", "B"] → ["A", "B"]', () => {
    expect(mergeScopePrefixes(['A', 'B'])).toEqual(['A', 'B'])
  })

  it('sibling paths under same root: ["A/B", "A/C"] → ["A/B", "A/C"]', () => {
    expect(mergeScopePrefixes(['A/B', 'A/C'])).toEqual(['A/B', 'A/C'])
  })

  it('deeply nested dedup: ["A/B/C/D", "A/B/C"] → ["A/B/C"]', () => {
    expect(mergeScopePrefixes(['A/B/C/D', 'A/B/C'])).toEqual(['A/B/C'])
  })

  it('triple nesting dedup: ["A/B/C", "A/B", "A"] → ["A"]', () => {
    expect(mergeScopePrefixes(['A/B/C', 'A/B', 'A'])).toEqual(['A'])
  })

  it('multiple trailing slashes: ["A//", "A//B"] → ["A"]', () => {
    expect(mergeScopePrefixes(['A//', 'A//B'])).toEqual(['A'])
  })

  it('whitespace trimmed: [" A", " A/B"] → ["A"]', () => {
    expect(mergeScopePrefixes([' A', ' A/B'])).toEqual(['A'])
  })

  // Windows 反斜杠来源（文件树 rel / 拖拽 / 旧会话存量）必须与正斜杠同语义，
  // 且输出统一为正斜杠形式
  it('backslash separator: ["A\\\\B", "A\\\\B\\\\C"] → ["A/B"]', () => {
    expect(mergeScopePrefixes(['A\\B', 'A\\B\\C'])).toEqual(['A/B'])
  })

  it('mixed separators: ["A/B", "A\\\\B\\\\C"] → ["A/B"]', () => {
    expect(mergeScopePrefixes(['A/B', 'A\\B\\C'])).toEqual(['A/B'])
  })

  it('trailing backslash: ["A\\\\", "A\\\\B"] → ["A"]', () => {
    expect(mergeScopePrefixes(['A\\', 'A\\B'])).toEqual(['A'])
  })
})