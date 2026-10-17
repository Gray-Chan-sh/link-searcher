#!/usr/bin/env node
// 编译飞书 bot sidecar，并按 Tauri `bundle.externalBin` 的命名要求放到 `src-tauri/binaries/`。
//
// 为什么必须有这一步：`externalBin` 要求文件叫 `<name>-<target-triple>[.exe]`，
// 而 `cargo` 产出的名字**没有** triple 后缀；**配了 externalBin 却缺文件会直接
// 打断 `tauri dev` / `tauri build`**（所以脚本挂在 `predev` / `prebuild` 上自动跑，
// 而不是等人记得手动执行）。
//
// 用 `-p lark-bot`（不是 `--bin`）：bot 是**独立 crate**，这样**完全不触碰主程序** ——
// 否则"生成 sidecar"要先编主程序，而主程序的 build.rs 又会校验 externalBin → 鸡生蛋
// （2026-10-15 实测撞上，见 docs/LARK_BOT.md §5.7）。
//
// ⚠️ 本文件**必须入库**：`.gitignore` 的 `scripts/*` 曾把它静默挡在仓库外 →
//    本地一切正常、CI 全新检出直接 `MODULE_NOT_FOUND`（2026-10-17 v1.3.0 发布失败的
//    根因）。改 `.gitignore` 时务必保留 `!scripts/build-sidecar.mjs` 例外。
//
// 目标三元组（决定产物叫什么名字 —— Tauri 按 **构建目标** 找 `<name>-<triple>` 文件）：
//   1. `--target <triple>` 参数；
//   2. `SIDECAR_TARGET` 环境变量（**CI 交叉编译**用：workflow 把它设为 `matrix.target`，
//      否则 macOS 的 x86_64 交叉构建会拿到按 host（arm64）命名的产物 → 找不到）；
//   3. 默认 = `rustc -vV` 的 host（本地开发）。
//   triple == host 时**不加** `--target`（产物在 `target/<profile>/`，与 `tauri dev`
//   消费的位置一致）；不同时加（产物在 `target/<triple>/<profile>/`）。
//
// 用法：
//   node scripts/build-sidecar.mjs                                          # debug（开发用，host）
//   node scripts/build-sidecar.mjs --release                                # release（打包用，host）
//   node scripts/build-sidecar.mjs --release --target x86_64-apple-darwin  # 交叉（CI 等价形态）

import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'

const release = process.argv.includes('--release')
const profile = release ? 'release' : 'debug'
const exeExt = process.platform === 'win32' ? '.exe' : ''

/** 取 `--key value` 形式参数的值（本脚本参数极少，不引解析库）。 */
function argValue(key) {
  const i = process.argv.indexOf(key)
  return i >= 0 ? process.argv[i + 1] : undefined
}

// 1. 目标三元组：`--target` 参数 > `SIDECAR_TARGET` 环境变量 > rustc host。
const rustcOut = execFileSync('rustc', ['-vV'], { encoding: 'utf8' })
const host = rustcOut
  .split('\n')
  .find((line) => line.startsWith('host:'))
  ?.replace('host:', '')
  .trim()
if (!host) {
  console.error('[sidecar] 无法从 `rustc -vV` 解析 host 三元组')
  process.exit(1)
}
const target = (argValue('--target') || process.env.SIDECAR_TARGET || host).trim()
if (!target) {
  console.error('[sidecar] `--target` / `SIDECAR_TARGET` 是空值')
  process.exit(1)
}

// 2. 编译 bot（**独立 crate**，不触及主程序：见文件头）。
const cross = target !== host
const cargoArgs = [
  'build',
  '--manifest-path',
  'src-tauri/Cargo.toml',
  '-p',
  'lark-bot',
  ...(release ? ['--release'] : []),
  ...(cross ? ['--target', target] : []),
]
console.log(`[sidecar] cargo ${cargoArgs.join(' ')}`)
execFileSync('cargo', cargoArgs, { stdio: 'inherit' })

// 3. 复制成 externalBin 要求的名字：`<name>-<triple>[.exe]`。
const src = cross
  ? join('src-tauri', 'target', target, profile, `lark-bot${exeExt}`)
  : join('src-tauri', 'target', profile, `lark-bot${exeExt}`)
if (!existsSync(src)) {
  console.error(`[sidecar] 编译产物不存在：${src}`)
  process.exit(1)
}
const outDir = join('src-tauri', 'binaries')
mkdirSync(outDir, { recursive: true })
const dest = join(outDir, `lark-bot-${target}${exeExt}`)
copyFileSync(src, dest)
console.log(`[sidecar] ${src} -> ${dest}`)
