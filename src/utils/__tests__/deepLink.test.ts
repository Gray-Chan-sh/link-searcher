import { describe, it, expect } from 'vitest'
import { parseDeepLink } from '../deepLink'

describe('parseDeepLink', () => {
  it('没有相关参数时返回 null（桌面端 / 直接打开 WebUI 的老行为）', () => {
    expect(parseDeepLink('')).toBeNull()
    expect(parseDeepLink('foo=1&bar=2')).toBeNull()
  })

  it('只带 origin 时不指明会话', () => {
    expect(parseDeepLink('origin=lark')).toEqual({ origin: 'lark', session: null })
  })

  it('飞书卡片深链：origin + session 都取到', () => {
    expect(parseDeepLink('origin=lark&session=3d3a9b1e-8c03-4fe3-a448-142ff70dceeb')).toEqual({
      origin: 'lark',
      session: '3d3a9b1e-8c03-4fe3-a448-142ff70dceeb',
    })
  })

  it('未知 origin 只当没有 —— 不因为一个拼错的参数就认它是深链', () => {
    expect(parseDeepLink('origin=evil')).toBeNull()
  })

  it('未知 origin + 合法 session：仍直达，但页面端退回 web 默认', () => {
    expect(parseDeepLink('origin=evil&session=abc')).toEqual({ origin: 'web', session: 'abc' })
  })

  it('origin 大小写与空白都被收敛', () => {
    expect(parseDeepLink('origin=%20LARK%20')).toEqual({ origin: 'lark', session: null })
  })

  it('session 去空白；纯空白视为没有', () => {
    expect(parseDeepLink('origin=lark&session=%20%20abc%20')).toEqual({ origin: 'lark', session: 'abc' })
    expect(parseDeepLink('session=%20%20')).toBeNull()
  })

  it('session 里的百分号编码被解码（id 本身再含特殊字符也不会串味）', () => {
    expect(parseDeepLink('session=a%2Fb%3Fc')).toEqual({ origin: 'web', session: 'a/b?c' })
  })
})
