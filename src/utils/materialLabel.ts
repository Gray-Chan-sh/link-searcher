const ELLIPSIS = '…'

function charWidth(ch: string): number {
  const cp = ch.codePointAt(0) ?? 0
  return cp > 0x7f ? 2 : 1
}

export function displayWidth(text: string): number {
  let width = 0
  for (const ch of text) width += charWidth(ch)
  return width
}

function takeHead(text: string, maxWidth: number): string {
  let width = 0
  let out = ''
  for (const ch of text) {
    const w = charWidth(ch)
    if (width + w > maxWidth) break
    width += w
    out += ch
  }
  return out
}

function takeTail(text: string, maxWidth: number): string {
  const chars = [...text]
  let width = 0
  let start = chars.length
  for (let i = chars.length - 1; i >= 0; i -= 1) {
    const w = charWidth(chars[i]!)
    if (width + w > maxWidth) break
    width += w
    start = i
  }
  return chars.slice(start).join('')
}

export function basename(path: string): string {
  if (!path) return ''
  const trimmed = path.replace(/[\\/]+$/, '')
  if (!trimmed) return ''
  const idx = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'))
  return trimmed.slice(idx + 1)
}

export function dirname(path: string): string {
  if (!path) return ''
  const trimmed = path.replace(/[\\/]+$/, '')
  const idx = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'))
  return idx > 0 ? trimmed.slice(0, idx) : ''
}

export function truncateForDisplay(name: string, maxWidth = 14): string {
  if (displayWidth(name) <= maxWidth) return name
  const dot = name.lastIndexOf('.')
  const hasExt = dot > 0 && dot < name.length - 1
  const ext = hasExt ? name.slice(dot) : ''
  const stem = hasExt ? name.slice(0, dot) : name
  const budget = maxWidth - displayWidth(ext) - displayWidth(ELLIPSIS)
  if (budget < 2) return takeHead(name, Math.max(1, maxWidth - displayWidth(ELLIPSIS))) + ELLIPSIS
  const head = Math.ceil(budget / 2)
  const tail = budget - head
  return takeHead(stem, head) + ELLIPSIS + takeTail(stem, tail) + ext
}

export function disambiguate(paths: string[]): Map<string, string> {
  const unique = [...new Set(paths.filter(p => p.length > 0))]
  const groups = new Map<string, string[]>()
  for (const p of unique) {
    const b = basename(p)
    const g = groups.get(b)
    if (g) g.push(p)
    else groups.set(b, [p])
  }
  const labels = new Map<string, string>()
  for (const [b, group] of groups) {
    const shortBase = truncateForDisplay(b)
    if (group.length === 1) {
      labels.set(group[0]!, shortBase)
      continue
    }
    for (const p of group) {
      const segs = p.split(/[\\/]+/).filter(Boolean)
      let prefix = ''
      for (let n = 2; n <= segs.length; n += 1) {
        const cand = segs.slice(-n, -1).join('/')
        const collides = group.some(q => {
          if (q === p) return false
          const qs = q.split(/[\\/]+/).filter(Boolean)
          return qs.slice(-n, -1).join('/') === cand
        })
        if (!collides) {
          prefix = cand
          break
        }
      }
      labels.set(p, prefix ? `${prefix}/${shortBase}` : shortBase)
    }
  }
  return labels
}
