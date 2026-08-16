// translateErr 单测：Node 原生 assert + type-stripping，与 scopeParser.test.ts 同机制。
import assert from 'node:assert'
import { translateErr } from '../translateErr.ts'

// 模拟 zh/en：zh 用户 t() 命中键 → 值即原文（不回归）；en 用户 t() 返回英文；未命中键回退原文
const zhT = (k: string) => k === 'err_empty_question' ? '问题不能为空' : k
const enT = (k: string) => k === 'err_empty_question' ? 'Question cannot be empty' : k
const noopT = (k: string) => k

// 已映射消息：zh 得到原文（t 命中时）
assert.equal(
  translateErr('问题不能为空', zhT),
  '问题不能为空',
  'zh 命中映射键应得到原文',
)

// 已映射消息：en 得到英文
assert.equal(
  translateErr('问题不能为空', enT),
  'Question cannot be empty',
  'en 应得到翻译后消息',
)

// 未映射消息：回退原文
assert.equal(
  translateErr('some-unmapped-error', enT),
  'some-unmapped-error',
  '未映射消息应原样返回',
)

// 键存在但 t() 未命中（翻译缺失）：回退原文而非键名
assert.equal(
  translateErr('AI 请求失败（检查网关配置或网络）', noopT),
  'AI 请求失败（检查网关配置或网络）',
  't() 未命中时应回退原文而非键名',
)

// 多键同义：未配置两条消息都映射到同一键（此处验证映射存在、t() 未命中时回退原文而非键名）
assert.equal(
  translateErr('AI 服务未配置，请在设置页填写 API Base URL', noopT),
  'AI 服务未配置，请在设置页填写 API Base URL',
  't() 未命中应回退原文',
)
assert.notEqual(
  translateErr('AI 服务未配置，请在设置页填写 API Base URL', noopT),
  'err_ai_not_configured',
  't() 未命中不得显示键名',
)

// 空字符串消息：回退原文
assert.equal(translateErr('', enT), '', '空消息应原样返回')

console.log('ALL translateErr assertions PASSED')