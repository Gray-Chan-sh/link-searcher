import { describe, it, expect } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import ReactMarkdown from 'react-markdown'
import { createMarkdownComponents, highlightChildren, highlightText, MD_REMARK_PLUGINS } from '../markdown.tsx'

function render(md: string, query = ''): string {
  return renderToStaticMarkup(
    <ReactMarkdown remarkPlugins={MD_REMARK_PLUGINS} components={createMarkdownComponents(query)}>
      {md}
    </ReactMarkdown>,
  )
}

describe('highlightText', () => {
  it('returns the text untouched for an empty query', () => {
    expect(highlightText('城市更新', '')).toEqual(['城市更新'])
  })

  it('escapes regex metacharacters in the query', () => {
    // 不应抛异常，也不应把 "(b" 当成分组
    const out = highlightText('a(b) c', '(b')
    expect(JSON.stringify(out)).toContain('mark')
  })
})

describe('highlightChildren', () => {
  it('leaves elements alone when the query is empty', () => {
    const nodes = highlightChildren(['a', 'b'], '')
    expect(nodes).toEqual(['a', 'b'])
  })

  it('wraps string children of nested elements', () => {
    const out = renderToStaticMarkup(<p>{highlightChildren(<strong>城市更新</strong>, '城市')}</p>)
    expect(out).toContain('<strong>')
    expect(out).toContain('<mark')
    expect(out).toContain('城市')
  })
})

describe('markdown rendering', () => {
  it('renders bold instead of showing ** markers', () => {
    const html = render('**关于深入推进城镇低效用地再开发**的指导意见')
    expect(html).toContain('<strong>')
    expect(html).not.toContain('**')
  })

  it('renders headings and lists as structure', () => {
    const html = render('# 一级标题\n\n- 列表项\n- 第二项')
    expect(html).toContain('<h1>')
    expect(html).toContain('<li>')
    expect(html).not.toContain('# 一级标题')
  })

  it('keeps search highlighting inside the rendered markup', () => {
    const html = render('**坚持政府引导、规划先行。**建立健全工作机制。', '政府引导')
    expect(html).toContain('<strong>')
    expect(html).toContain('<mark')
    expect(html).toContain('政府引导')
  })

  it('wraps tables in a horizontally scrollable container', () => {
    const html = render('| 项目 | 金额 |\n|---|---|\n| 土地 | 100 |')
    expect(html).toContain('overflow-x-auto')
    expect(html).toContain('<table>')
    expect(html).toContain('土地')
  })

  it('renders links as non-clickable text (no navigation out of the webview)', () => {
    const html = render('[土地管理法](https://example.com/law)')
    expect(html).toContain('土地管理法')
    expect(html).not.toContain('href=')
    expect(html).not.toContain('<a')
  })

  it('renders images as placeholders (no network / base64 loads)', () => {
    const html = render('![示意图](https://example.com/a.png)')
    expect(html).toContain('[图片：示意图]')
    expect(html).not.toContain('<img')
  })
})
