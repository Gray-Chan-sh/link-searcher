// scopeParser 纯函数断言测试（Node 原生 type-stripping 单测，不依赖框架）
import assert from 'node:assert'
import { parseScope } from '../scopeParser.ts'

// S1: /ext + @ 文件混用
const r1 = parseScope('根据@财务/年度财务报告.md /ext:pdf 汇总收入')
assert.deepStrictEqual(r1.scope.mention_files, ['财务/年度财务报告.md'], 'S1: @文件提取')
assert.deepStrictEqual(r1.scope.conditions, [{ kind: 'ext', value: 'pdf' }], 'S1: /ext 提取')
assert.equal(r1.cleanText, '根据 汇总收入', 'S1: token 剥离')
assert.equal(r1.scopeAction, null, 'S1: 无范围动作')

// S2: 无命令时 conditions 空，行为=现状
const r2 = parseScope('为什么营收下降')
assert.deepStrictEqual(r2.scope.conditions, [], 'S2: conditions 空')
assert.equal(r2.cleanText, '为什么营收下降', 'S2: 原文保留')

// S3: /date
const r3 = parseScope('/date:2025-01-01~2025-12-31 收据')
assert.deepStrictEqual(r3.scope.conditions[0], { kind: 'date', value: '2025-01-01~2025-12-31' }, 'S3: /date')

// S4: /范围:全库 → clear
const r4 = parseScope('/范围:全库 重新回答')
assert.equal(r4.scopeAction, 'clear', 'S4: 范围清除动作')

// S5: /范围:目录
const r5 = parseScope('/范围:财务 本季度')
assert.equal(r5.scopeAction, 'dir:财务', 'S5: 目录范围动作')

// S6: @目录 vs @文件 区分
const r6 = parseScope('@财务 与 @年度报告.md 对比')
assert.deepStrictEqual(r6.scope.mention_dirs, ['财务'], 'S6: 目录')
assert.deepStrictEqual(r6.scope.mention_files, ['年度报告.md'], 'S6: 文件')

// S7: @上轮 继承
const r7 = parseScope('@上轮 那净利润呢')
assert.deepStrictEqual(r7.scope.inherit_from, [-1], 'S7: 继承最近轮')

// S8: /模糊 条件暂存原文（待 LLM 解析）
const r8 = parseScope('/模糊:跟去年收购相关的 有哪些')
assert.deepStrictEqual(r8.scope.conditions[0], { kind: 'fuzzy', value: '跟去年收购相关的' }, 'S8: 模糊条件暂存')

// S9: URL 不破坏（M1 回归）
const r9 = parseScope('帮我总结 https://example.com/report.pdf 的内容')
assert.equal(r9.cleanText, '帮我总结 https://example.com/report.pdf 的内容', 'S9: URL 完整保留')
assert.deepStrictEqual(r9.scope.conditions, [], 'S9: URL 不产生条件')

// S10: @@ 双符号前缀污染（M2 回归）
const r10 = parseScope('对比 @@财务 和 健康')
assert.deepStrictEqual(r10.scope.mention_dirs, ['财务'], 'S10: @@ 不应带入 @ 前缀')
assert.equal(r10.cleanText, '对比 和 健康', 'S10: @@ 残留清理干净')

// S11: 句尾孤立 @ 不报错、不产生 mention
const r11 = parseScope('问题 @')
assert.deepStrictEqual(r11.scope.mention_files, [], 'S11: 句尾 @ 无 mention_files')
assert.deepStrictEqual(r11.scope.mention_dirs, [], 'S11: 句尾 @ 无 mention_dirs')
assert.equal(r11.cleanText, '问题', 'S11: 孤立 @ 清理')

// S12: 路径含空格，以文件扩展名结尾（M3 回归 — macOS 路径含空格）
const r12 = parseScope('@案件/WC 万城/诉讼案件/音字转换文字记录（20260326）.pdf ，第三人庄建军是否说过不参与公司的经营管理。')
assert.deepStrictEqual(r12.scope.mention_files, ['案件/WC 万城/诉讼案件/音字转换文字记录（20260326）.pdf'], 'S12: 含空格路径完整提取')
assert.equal(r12.cleanText, '，第三人庄建军是否说过不参与公司的经营管理。', 'S12: token 剥离干净')

// S13: 目录路径不含空格保持原有行为
const r13 = parseScope('@财务 本季度收入')
assert.deepStrictEqual(r13.scope.mention_dirs, ['财务'], 'S13: 目录无空格保持')
assert.equal(r13.cleanText, '本季度收入', 'S13: 目录 token 剥离')

console.log('ALL scopeParser assertions PASSED')