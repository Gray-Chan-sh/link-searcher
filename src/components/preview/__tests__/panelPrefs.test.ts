import { describe, it, expect } from 'vitest'
import {
  clampPreviewWidth,
  imageSrc,
  normalizeFontSize,
  stepFontSize,
  DEFAULT_PREVIEW_FONT_SIZE,
  DEFAULT_PREVIEW_WIDTH,
  MAX_PREVIEW_WIDTH,
  MIN_PREVIEW_WIDTH,
} from '../panelPrefs.ts'

describe('normalizeFontSize', () => {
  it('keeps an exact step', () => expect(normalizeFontSize(16)).toBe(16))
  it('snaps to the nearest step (ties prefer the smaller one)', () => {
    expect(normalizeFontSize(15)).toBe(14)
    expect(normalizeFontSize(17)).toBe(16)
  })
  it('falls back to the default for junk', () => {
    expect(normalizeFontSize('nope')).toBe(DEFAULT_PREVIEW_FONT_SIZE)
    expect(normalizeFontSize(null)).toBe(DEFAULT_PREVIEW_FONT_SIZE)
    expect(normalizeFontSize(undefined)).toBe(DEFAULT_PREVIEW_FONT_SIZE)
    expect(normalizeFontSize('')).toBe(DEFAULT_PREVIEW_FONT_SIZE)
    expect(normalizeFontSize(Number.NaN)).toBe(DEFAULT_PREVIEW_FONT_SIZE)
  })
  it('parses numeric strings (localStorage round-trip)', () => {
    expect(normalizeFontSize('18')).toBe(18)
  })
})

describe('stepFontSize', () => {
  it('steps up and down one notch', () => {
    expect(stepFontSize(14, 1)).toBe(16)
    expect(stepFontSize(14, -1)).toBe(12)
  })
  it('clamps at the ends', () => {
    expect(stepFontSize(18, 1)).toBe(18)
    expect(stepFontSize(12, -1)).toBe(12)
  })
  it('normalizes before stepping', () => {
    expect(stepFontSize(100, -1)).toBe(16)
    expect(stepFontSize('junk', 1)).toBe(16)
  })
})

describe('clampPreviewWidth', () => {
  it('defaults when value is junk', () => {
    expect(clampPreviewWidth(undefined, 1600)).toBe(DEFAULT_PREVIEW_WIDTH)
    expect(clampPreviewWidth(Number.NaN, 1600)).toBe(DEFAULT_PREVIEW_WIDTH)
  })
  it('enforces the absolute limits', () => {
    expect(clampPreviewWidth(5000, 2560)).toBe(MAX_PREVIEW_WIDTH)
    expect(clampPreviewWidth(10, 1600)).toBe(MIN_PREVIEW_WIDTH)
  })
  it('never exceeds 60% of the viewport', () => {
    // 800 × 0.6 = 480 —— 比绝对上限小，所以按视口算
    expect(clampPreviewWidth(700, 800)).toBe(480)
  })
  it('keeps the minimum width even on a tiny viewport', () => {
    expect(clampPreviewWidth(400, 300)).toBe(MIN_PREVIEW_WIDTH)
  })
  it('rounds to whole pixels', () => {
    expect(clampPreviewWidth(400.6, 1600)).toBe(401)
  })
})

describe('imageSrc', () => {
  const toAsset = (p: string) => `asset://localhost/${p}`

  it('prefers inline base64 with the right mime type', () => {
    expect(imageSrc({ image_base64: 'AAA', image_path: 'C:\\a\\b.PNG' }, toAsset)).toBe('data:image/png;base64,AAA')
  })

  it('falls back to an unknown extension as jpeg', () => {
    expect(imageSrc({ image_base64: 'AAA', image_path: 'C:\\a\\b.xyz' }, toAsset)).toBe('data:image/jpeg;base64,AAA')
  })

  it('uses the asset url when there is no base64', () => {
    expect(imageSrc({ image_base64: null, image_path: '/a/b.png' }, toAsset)).toBe('asset://localhost//a/b.png')
  })

  it('returns empty when neither is present', () => {
    expect(imageSrc({ image_base64: null, image_path: null }, toAsset)).toBe('')
  })
})
