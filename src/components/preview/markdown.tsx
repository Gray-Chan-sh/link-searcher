/**
 * 预览面板的 Markdown 渲染配置（与 AI 聊天共用同一套插件与语义）。
 *
 * 分成独立文件的原因：渲染逻辑（在**已解析的 AST 元素**里插入 `<mark>`、
 * 表格套横向滚动、图片/链接降级）是纯函数，可以脱离面板单测
 * （见 `__tests__/markdown.test.tsx`）。
 */
import { Children, cloneElement, isValidElement } from 'react'
import type { Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import remarkCjkFriendly from 'remark-cjk-friendly'

/** 与 ChatPanel 一致：GFM（表格/删除线）+ 中文换行友好。 */
export const MD_REMARK_PLUGINS = [remarkGfm, remarkCjkFriendly]

/**
 * 纯文本高亮：按空格拆成多个词，转义正则元字符（最多 20 个）。
 * 输入是用户搜索框内容，不存在正则注入风险。
 */
export function highlightText(text: string, query: string): React.ReactNode[] {
  if (!query.trim()) return [text]
  const terms = query.split(/\s+/).filter(t => t.length > 0).slice(0, 20)
  if (terms.length === 0) return [text]
  const termSet = new Set(terms.map(t => t.toLowerCase()))
  const escaped = terms.map(t => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
  const regex = new RegExp(`(${escaped.join('|')})`, 'gi')
  const parts = text.split(regex)
  return parts.map((part, i) =>
    part.length > 0 && termSet.has(part.toLowerCase())
      ? <mark key={i} className="bg-yellow-200 dark:bg-yellow-700/50 rounded px-0.5">{part}</mark>
      : part,
  )
}

/**
 * 在**已解析的 Markdown 元素树**里就地高亮：只替换纯文本节点，
 * 不会破坏标题/表格/列表结构。
 */
export function highlightChildren(children: React.ReactNode, query: string): React.ReactNode {
  if (!query.trim()) return children
  return Children.map(children, child => {
    if (typeof child === 'string') return highlightText(child, query)
    if (typeof child === 'number') return highlightText(String(child), query)
    if (isValidElement<{ children?: React.ReactNode }>(child)) {
      const kids = child.props.children
      if (kids === undefined) return child
      return cloneElement(child, undefined, highlightChildren(kids, query))
    }
    return child
  })
}

/**
 * 给 `ReactMarkdown` 用的元素覆写：
 * - 文本类元素走 `highlightChildren`（搜索词在排版后仍高亮）；
 * - 表格套横向滚动容器，宽表不撑破窄面板；
 * - 图片显示 `[图片：alt]` 占位、链接渲染为不可点文本 —— 预览里点链接
 *   会把 Tauri WebView 导航走，而且远程图片会触发网络请求。
 */
export function createMarkdownComponents(query: string): Components {
  const hi = (children: React.ReactNode) => highlightChildren(children, query)
  return {
    p: ({ children }) => <p>{hi(children)}</p>,
    li: ({ children }) => <li>{hi(children)}</li>,
    td: ({ children }) => <td>{hi(children)}</td>,
    th: ({ children }) => <th>{hi(children)}</th>,
    h1: ({ children }) => <h1>{hi(children)}</h1>,
    h2: ({ children }) => <h2>{hi(children)}</h2>,
    h3: ({ children }) => <h3>{hi(children)}</h3>,
    h4: ({ children }) => <h4>{hi(children)}</h4>,
    h5: ({ children }) => <h5>{hi(children)}</h5>,
    h6: ({ children }) => <h6>{hi(children)}</h6>,
    blockquote: ({ children }) => <blockquote>{hi(children)}</blockquote>,
    a: ({ href, children }) => (
      <span className="text-blue-600 dark:text-blue-400 underline decoration-dotted" title={href}>
        {hi(children)}
      </span>
    ),
    img: ({ alt }) => <span className="text-xs text-gray-400">[图片{alt ? `：${alt}` : ''}]</span>,
    table: ({ children }) => <div className="overflow-x-auto"><table>{children}</table></div>,
  }
}
