import { describe, it, expect } from 'vitest'
import { translateErr } from '../translateErr'

// 模拟 zh/en：zh 用户 t() 命中键 → 值即原文（不回归）；en 用户 t() 返回英文；未命中键回退原文
const zhT = (k: string) => (k === 'err_empty_question' ? '问题不能为空' : k)
const enT = (k: string) => (k === 'err_empty_question' ? 'Question cannot be empty' : k)
const noopT = (k: string) => k

describe('translateErr', () => {
  it('zh 命中映射键应得到原文', () => {
    expect(translateErr('问题不能为空', zhT)).toBe('问题不能为空')
  })

  it('en 应得到翻译后消息', () => {
    expect(translateErr('问题不能为空', enT)).toBe('Question cannot be empty')
  })

  it('未映射消息应原样返回', () => {
    expect(translateErr('some-unmapped-error', enT)).toBe('some-unmapped-error')
  })

  it('t() 未命中时应回退原文而非键名', () => {
    expect(translateErr('AI 请求失败（检查网关配置或网络）', noopT)).toBe('AI 请求失败（检查网关配置或网络）')
    expect(translateErr('AI 服务未配置，请在设置页填写 API Base URL', noopT)).not.toBe('err_ai_not_configured')
  })

  it('空消息应原样返回', () => {
    expect(translateErr('', enT)).toBe('')
  })
})
