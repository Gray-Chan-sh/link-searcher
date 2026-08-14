// 范围解析纯函数模块：从输入文本提取 @mention 与 /命令（/ext /date /范围 /模糊）。
// 纯函数、无依赖，便于单测。返回剥离 token/命令后的干净文本 + scope。

export interface ScopeCondition {
  kind: string
  value: string
  parsed?: string | null
}

export interface TurnScope {
  mention_files: string[]
  mention_dirs: string[]
  inherit_from: number[]
  conditions: ScopeCondition[]
}

// 匹配 @mention：
//   - 优先匹配带空格的文件路径（以文件扩展名结尾）
//   - 回退匹配无空格路径（目录/简短文件）
// 空格在路径中合法（macOS 文件名），但中文标点 、。？！ 是路径与自然语言的分界
const MENTION_RE = /@((?:[^\s@，。？！；:、,?:;]+(?:\s[^\s@，。？！；:、,?:;]+)*\.\w{1,6})|(?:[^\s@，。？！；:、,?:;]+))/g
const CMD_EXT_RE = /\/ext:([^\s，。？！；:、,?:;]+)/g
const CMD_DATE_RE = /\/date:([^\s，。？！；:、,?:;]+)/g
const CMD_SCOPE_RE = /\/范围[:：]([^\s，。？！；:、,?:;]+)/g
const CMD_FUZZY_RE = /\/模糊[:：]([^\s，。？！；:、,?:;]+)/g

export function isFileLike(path: string): boolean {
  return /\.\w{1,6}$/.test(path)
}

export interface ParsedScope {
  scope: TurnScope
  cleanText: string
  /** /范围 解析出的会话级动作：'clear'(全库) 或 'dir:xxx'(设置目录范围)，无则为 null */
  scopeAction: string | null
}

export function parseScope(text: string): ParsedScope {
  const scope: TurnScope = {
    mention_files: [],
    mention_dirs: [],
    inherit_from: [],
    conditions: [],
  }
  let clean = text
  let scopeAction: string | null = null

  let m: RegExpExecArray | null

  // @mention：文件/目录/轮次继承
  while ((m = MENTION_RE.exec(text)) !== null) {
    const raw = m[1].trim()
    if (!raw) continue
    if (raw === '上轮') {
      scope.inherit_from.push(-1) // -1 = 最近一轮
    } else if (/^第\d+轮$/.test(raw)) {
      const n = parseInt(raw.slice(1, -1), 10)
      if (n >= 1) scope.inherit_from.push(n - 1)
    } else if (isFileLike(raw)) {
      if (!scope.mention_files.includes(raw)) scope.mention_files.push(raw)
    } else {
      if (!scope.mention_dirs.includes(raw)) scope.mention_dirs.push(raw)
    }
    clean = clean.replaceAll(`@${raw}`, '')
  }

  // /ext:pdf
  while ((m = CMD_EXT_RE.exec(text)) !== null) {
    const v = m[1].trim().toLowerCase()
    if (v && !scope.conditions.some(c => c.kind === 'ext' && c.value === v)) {
      scope.conditions.push({ kind: 'ext', value: v })
    }
    clean = clean.replace(m[0], '')
  }

  // /date:2025-01-01~2025-12-31
  while ((m = CMD_DATE_RE.exec(text)) !== null) {
    const v = m[1].trim()
    if (v && !scope.conditions.some(c => c.kind === 'date' && c.value === v)) {
      scope.conditions.push({ kind: 'date', value: v })
    }
    clean = clean.replace(m[0], '')
  }

  // /范围:全库 或 /范围:目录路径
  while ((m = CMD_SCOPE_RE.exec(text)) !== null) {
    const v = m[1].trim()
    if (v) scopeAction = v === '全库' ? 'clear' : `dir:${v}`
    clean = clean.replace(m[0], '')
  }

  // /模糊:自由文本（条件，待 LLM 解析，value 暂存原文）
  while ((m = CMD_FUZZY_RE.exec(text)) !== null) {
    const v = m[1].trim()
    if (v && !scope.conditions.some(c => c.kind === 'fuzzy' && c.value === v)) {
      scope.conditions.push({ kind: 'fuzzy', value: v })
    }
    clean = clean.replace(m[0], '')
  }

  // 清理多余空白与残留的孤立 @（合法 mention 已在上面剥离，剩下的 @ 是误输入）
  clean = clean.replace(/@+/g, ' ').replace(/\s+/g, ' ').trim()
  return { scope, cleanText: clean, scopeAction }
}