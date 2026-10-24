/**
 * 预览面板的偏好（字号档位 / 宽度）——纯函数，方便单测。
 *
 * 面板宽度与正文字号都记忆在 localStorage：用户把面板拖宽、把字调大之后，
 * 切页/重启仍然保留（此前宽度只存在组件 state 里，刷新即回 400px）。
 */

/** 正文字号档位（px）。默认 14，四档即可覆盖「字太小」到「要大屏放大」。 */
export const PREVIEW_FONT_STEPS = [12, 14, 16, 18] as const

export const DEFAULT_PREVIEW_FONT_SIZE = 14
export const DEFAULT_PREVIEW_WIDTH = 420
export const MIN_PREVIEW_WIDTH = 280
/** 宽度绝对上限；同时受 `视口宽 × MAX_WIDTH_VIEWPORT_RATIO` 约束。 */
export const MAX_PREVIEW_WIDTH = 900
export const MAX_WIDTH_VIEWPORT_RATIO = 0.6

export const LS_PREVIEW_FONT_SIZE = 'ls_preview_font_size'
export const LS_PREVIEW_WIDTH = 'ls_preview_width'

/** 把任意存储值吸附到最近的档位（相邻两档中间时取小的一档）；非法值回默认。 */
export function normalizeFontSize(value: unknown): number {
  const n = typeof value === 'number'
    ? value
    : typeof value === 'string' && value.trim() !== ''
      ? Number(value)
      : Number.NaN
  if (!Number.isFinite(n)) return DEFAULT_PREVIEW_FONT_SIZE
  let best: number = PREVIEW_FONT_STEPS[0]
  for (const step of PREVIEW_FONT_STEPS) {
    if (Math.abs(step - n) < Math.abs(best - n)) best = step
  }
  return best
}

/** 上一档 / 下一档（到边界就停住，不循环）。 */
export function stepFontSize(current: unknown, dir: 1 | -1): number {
  const steps: readonly number[] = PREVIEW_FONT_STEPS
  const idx = steps.indexOf(normalizeFontSize(current))
  const next = Math.min(steps.length - 1, Math.max(0, idx + dir))
  return steps[next] ?? DEFAULT_PREVIEW_FONT_SIZE
}

/** 宽度限制在 [280, min(900, 60% 视口)]。视口本身很窄时以最小宽度为准。 */
export function clampPreviewWidth(value: unknown, viewportWidth: number): number {
  const n = Number(value)
  const base = Number.isFinite(n) ? n : DEFAULT_PREVIEW_WIDTH
  const vw = Number.isFinite(viewportWidth) && viewportWidth > 0 ? viewportWidth : 1440
  const max = Math.max(
    MIN_PREVIEW_WIDTH,
    Math.min(MAX_PREVIEW_WIDTH, Math.floor(vw * MAX_WIDTH_VIEWPORT_RATIO)),
  )
  return Math.min(max, Math.max(MIN_PREVIEW_WIDTH, Math.round(base)))
}

/** 预览图片的 src：优先 base64（浏览器模式下 asset URL 会拼出错误地址），回退 asset 协议。 */
export function imageSrc(preview: { image_base64: string | null; image_path: string | null }, toAssetUrl: (p: string) => string): string {
  if (preview.image_base64) {
    const ext = (preview.image_path?.split('.').pop() || 'jpeg').toLowerCase()
    const mime = ({ jpg: 'jpeg', jpeg: 'jpeg', png: 'png', gif: 'gif', webp: 'webp', bmp: 'bmp', tiff: 'tiff', tif: 'tiff' } as Record<string, string>)[ext] ?? 'jpeg'
    return `data:image/${mime};base64,${preview.image_base64}`
  }
  return preview.image_path ? toAssetUrl(preview.image_path) : ''
}

/**
 * 哪些扩展名的正文是 **Markdown**。
 *
 * 后端路由（`extractor/office/mod.rs`）：`.docx/.docm/.pptx/.../.odt/.ods/.odp/.rtf/.epub/.csv`
 * 走 `anydoc::to_markdown`；`.md/.markdown` 本身就是 Markdown。其余（pdf/txt/doc/xls/音频/图片）
 * 是纯文本，不做渲染。
 */
const MARKDOWN_EXTS = new Set([
  'docx', 'docm', 'pptx', 'pptm', 'ppsx', 'ppsm', 'pps', 'pot',
  'odt', 'ods', 'odp', 'rtf', 'epub', 'csv', 'md', 'markdown',
])

/** 按扩展名判断正文是否为 Markdown（大小写、前导点都容忍）。 */
export function isMarkdownSource(ext: string | null | undefined): boolean {
  if (!ext) return false
  return MARKDOWN_EXTS.has(ext.trim().toLowerCase().replace(/^\./, ''))
}
