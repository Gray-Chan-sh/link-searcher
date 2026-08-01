# Link-Searcher 变更日志

> 2026年7月30日 — 8月1日，共 35+ commit，修复 60+ Bug，完成 25+ 功能改进

---

## 2026-08-01 (今日)

### R4-C 前端质量改进
- **3-19 焦点判断改用 data-search-input**：SearchPage.tsx 原用 `placeholder.includes()` 判断焦点在搜索框，中文模式下失效。改为 `activeEl?.closest('[data-search-input]')`，与 SearchBar.tsx 的 `data-search-input="true"` 属性匹配（`src/pages/SearchPage.tsx`）
- **3-20 useIndexStatus 动态轮询间隔**：原固定 5s 轮询，改为 `is_scanning` 时 5s、空闲时 30s，减少索引空闲时的无效请求（`src/hooks/useIndexStatus.ts`）
- **3-22 SearchPage setTimeout 泄漏修复**：handleExport 内 3 个 `setTimeout` 无清理，用 `timersRef` + unmount effect 统一清理，防止组件卸载后状态更新崩溃（`src/pages/SearchPage.tsx`）
- **3-24 添加 ErrorBoundary**：新建 `src/components/ErrorBoundary.tsx`，在 App.tsx 外层包裹，渲染错误时显示"应用出错了，请重启或查看日志"而非白屏（`src/components/ErrorBoundary.tsx`、`src/App.tsx`）
- **3-25 暗色模式闪烁修复**：theme.tsx 原 `useState('light')` 初始值导致首次渲染闪烁。改为 `useMemo` 同步计算 resolved 值，DOM 初次渲染即正确（`src/theme.tsx`）
- **3-27 formatSize/formatTime 去重**：新建 `src/utils/format.ts` 统一工具函数，删除 PreviewPanel.tsx、ResultList.tsx 中的重复定义，减少维护成本（`src/utils/format.ts`、`src/components/PreviewPanel.tsx`、`src/components/ResultList.tsx`）
- **3-29 alert() 替换为 Tauri message()**：Settings.tsx 迁移失败提示原用浏览器 `alert()`，改为 `@tauri-apps/plugin-dialog` 的 `message()`，保持应用内 Dialog 风格一致（`src/pages/Settings.tsx`）

---

## 2026-08-01 (今日)

### 全量 i18n 改造
- **前端硬编码字符串全部提取到 en.ts / zh.ts**（`src/i18n/`）：新增 ~112 个翻译 key，覆盖 SearchPage、Browse、IndexStatus、DirManager、LogViewer、FileTypes、SearchBar、ResultList、PreviewPanel、FilterPanel、StatusBar、OnboardingWizard 共 12 个组件/页面
- **t() 支持参数**：`src/i18n/index.tsx` 扩展 `t(key, params?)` 签名，支持 `{placeholder}` 模板替换（如 `t('saved_to', { path })`、`t('results_count', { total })` 等）
- **SearchPage 键盘检查修复**：原检查 `placeholder.includes('your documents')` 在中文模式下失效，改为 `dataset.searchInput` 属性（`src/components/SearchBar.tsx` 加 `data-search-input="true"`，`src/pages/SearchPage.tsx` 改查该属性）
- **涉及文件**：`src/i18n/en.ts`、`src/i18n/zh.ts`、`src/i18n/index.tsx`、`src/pages/SearchPage.tsx`、`src/pages/Browse.tsx`、`src/pages/IndexStatus.tsx`、`src/pages/DirManager.tsx`、`src/pages/LogViewer.tsx`、`src/pages/FileTypes.tsx`、`src/components/SearchBar.tsx`、`src/components/ResultList.tsx`、`src/components/PreviewPanel.tsx`、`src/components/FilterPanel.tsx`、`src/components/StatusBar.tsx`、`src/components/OnboardingWizard.tsx`

---

## 2026-08-01

### 路径处理修复
- **to_relative 前缀误匹配**：原实现用 `path_str.starts_with(&root_str)` 字符串前缀比较，会把 `/tmp/foobar` 误认为 `/tmp/foo` 的子路径。改用 `Path::strip_prefix`（组件感知），并新增回归测试 `to_relative_respects_component_boundary`（`src-tauri/src/scanner/helpers.rs`）
- **路径迁移字节语义**：`migrate_paths_to_relative` 原用 SQL `SUBSTR(path, ?)` 按字节长度截断，中文等多字节路径会截错。改为 Rust 侧逐行迁移：按 `dir_id + prefix%` 查询后用 `path.strip_prefix(prefix)`（带 `/` 边界安全）更新（`src-tauri/src/db/tracker.rs`）

### 扫描统计与启动流程修复
- **扫描总耗时被覆盖**：`trigger_scan` 与 `rebuild_index` 中多目录扫描累加 `total_duration_ms = r.duration_ms` 每次都覆盖为最后一个目录的耗时，改为 `+=` 累加（`src-tauri/src/commands/index.rs`）
- **watcher 启动窗口期丢事件**：原启动流程先启动扫描线程、扫描完成后才发 `StartWatch`，扫描期间的文件变更因 watcher 未启动而丢失。改为先在主线程读取目录列表并发送 `StartWatch`，再启动扫描线程（`src-tauri/src/lib.rs`）
- **delete_file 静默吞错**：`mark_deleted` 失败被 `match` 静默忽略，改为 `if let Err(e)` 记录 `log::warn!`（`src-tauri/src/indexer.rs`）

### 搜索目录筛选修复
- **LIKE `%`/`_` 通配符转义**：dir_paths → file_ids 查询中 `p.replace('%', "%%")` 无效（SQLite LIKE 不识别 `%%`），改用 `ESCAPE '\'` 转义 `%` 和 `_`，避免含特殊字符的目录路径匹配错误。`search` 与 `export_search_results` 两处路径解析均已修复（`src-tauri/src/commands/search.rs`）

### TypeScript strict 模式
- **开启 TS strict 模式**：`tsconfig.app.json` 添加 `"strict": true`，符合 AGENTS.md 规范（strict + 禁止 any）。现有 34 个 TS 文件经 `tsc --noEmit -p tsconfig.app.json` 验证零错误
- **移除 SearchBar 中 `as any[]`**：`dropdown` 合并 suggestions（`string[]`）与 history（`SearchHistoryEntry[]`）改用展开语法 `[...suggestions, ...history]`，类型自然推断为 `(string | SearchHistoryEntry)[]`（`src/components/SearchBar.tsx`）

### 前端功能正确性修复
- **R3-2 预览高亮奇数次错乱**：`highlightText` 用带 `g` 标志的 `regex.test(part)` 判断是否高亮，`lastIndex` 状态导致奇数个匹配时高亮错乱。改用 `Set`（术语小写集合）做成员判断，正则仅用于切分（`src/components/PreviewPanel.tsx`）
- **R3-3 NumberField 清空输入回退异常**：`parseInt(e.target.value, 10) || min` 把空串/`NaN` 静默写成 `min` 且在输入过程中无法清空，改为 `Number.isNaN(v) ? min : Math.max(min, v)` NaN 安全钳制（`src/pages/Settings.tsx`）
- **R3-4 No Results 页 `<a href>` 整页跳转**：HashRouter 下 `<a href="/index">` 触发整页刷新，改用 `react-router-dom` 的 `<Link to="/index">`（`src/pages/SearchPage.tsx`）
- **R3-5 Enter 提交后 debounce 重复请求**：`submitSearch` 立即执行搜索后，300ms debounce effect 又因 query 变化触发一次同参数请求。新增 `lastSubmittedRef` 记录最近一次提交键 `query|page|sortField|sortOrder`，debounce effect 命中即跳过（`src/hooks/useSearch.ts`）
- **R3-14 Browse 搜索无防抖**：搜索框每个字符都触发一次 `listFilesDb` 请求。新增 `debouncedSearch` state + 300ms setTimeout 防抖，`loadFiles` 改用防抖后的值（`src/pages/Browse.tsx`）
- **R3-15 快速点击文件预览竞态**：慢返回覆盖快返回。新增 `previewVersionRef` 版本号，`selectFile` 每次自增并捕获本地版本，await 返回后版本不匹配则丢弃（`src/pages/Browse.tsx`）
- **R3-16 设置项每键写库**：`handleFieldChange` 每个字符都调 `updateSettings`，改用 `saveTimerRef` 300ms 防抖合并写入，卸载时清理未落盘的定时器（`src/pages/Settings.tsx`）

---

## 2026-07-30

### 项目初始化
- **ed1a639** Initial commit：Tauri 2 + React 19 + Tantivy 搜索引擎 + Tesseract OCR
- **874a0e4** chore：忽略 Tantivy 索引缓存文件

---

## 2026-07-31（第一轮：PaddleOCR + 启动流程 + Bug 修复）

### 🚀 PaddleOCR 内置引擎
- **`0e609c4`** feat: PaddleOCR 内置引擎 + 启动扫描 + 实时监控
  - 集成 `pure-onnx-ocr`（tract 纯 Rust ONNX 推理），PP-OCRv5 模型编译进二进制
  - 引擎优先级：PaddleOCR(默认) → Apple Vision → Windows OCR → Tesseract
  - `include_bytes!` 内嵌 21MB 模型，零外部依赖
  - 新增 `startup_scan()` 启动自动扫描
  - 实时文件监控（notify 300ms 防抖）
  - 文件移位检测（MD5 哈希匹配）
  - 默认排除规则（`#` `$` `.` `~` 前缀文件 + `.tmp` `.bak` 后缀等）
  - 移除全局快捷键 Ctrl+Space
  - 更新 README + USER_MANUAL

### 🔴 Bug 修复（12 项）`45db344`
1. `took_ms` 实为微秒 → `as_micros()` → `as_millis()`（searcher.rs）
2. `mem::forget(watcher)` 线程泄漏 → watcher 存入 AppState
3. MD5 哈希不一致（文件字节 vs 文本字节）→ 统一文件字节 MD5
4. `upsert_file` ON CONFLICT 错误重置 `indexed=0` → SQL 加 CASE WHEN
5. `last_scan` 秒 vs `mtime` 微秒精度不匹配 → `timestamp_micros()`
6. CSV 导出 path 列写成 file_name → SearchHit 加 path 字段
7. OCR 引擎检查与 PaddleOCR 默认冲突 → 匹配区分各引擎
8. FileWatcher 只处理 paths[0] → 遍历所有 paths
9. CSV 不转义特殊字符 → 所有列转义
10. `db_path.to_str().unwrap()` 非 ASCII 路径崩溃 → `to_string_lossy()`
11. OCR 预处理临时文件 PID 并发冲突 → UUID 替代 PID
12. macOS LibreOffice Dock 图标闪烁 → LSUIElement RAII guard（`bae64db`）

### 🏗️ 架构改进（16 项）
- **`c898d07`** 架构/性能/安全改进集
  - 定期 commit（每 100 文件自动提交）
  - IndexReader 复用（缓存 + reload）
  - `content_suggest` 字段用于搜索建议
  - `sort=name` Rust 侧排序
  - `filename:` 正则解析（支持任意位置）
  - CLI data_dir 统一
  - 移除非关键 unwrap/expect
  - PaddleOCR `Mutex + Send/Sync` 安全包装
  - 取消扫描功能（`cancel_scan` AtomicBool）
  - 清理孤儿 content_index
  - 数据库 VACUUM
- **`59bb801`** 流式MD5 + WalkDir 超时 + watcher 自动重连
  - MD5 流式计算（BufReader 替代 read_to_end）
  - 文件大小上限 100MB，超大文件只读首尾 1MB
  - WalkDir 计数 3 秒超时保护
  - FileWatcher 后台线程自动重连（3 次重试，500ms 间隔）
- **`75c7501`** Rayon 并行索引：`batch_index` par_iter 并行提取 + 串行 Tantivy 写入
- **`f82c645`** dead code 清理：`process_event`/`handle_create_modify`/`handle_delete`、`RawTokenizer`

### 🎨 前端假功能修复（8 项）`73489ef`
1. 排序选择器"死控件" → 打通前端→API→后端 sort/sortOrder
2. Pause/Resume 假按钮 → 改为取消扫描按钮
3. 文件类型分布假数据 → 新增 `get_file_type_stats` 命令
4. Recent Changes 计算错误 → 新增 `ScanDelta` 追踪真实数据
5. CSV 导出无保存对话框 → 系统 `save()` 对话框
6. DEBUG eprintln 遗留 → 删除

### 🟠 可用性改进（11 项）`789a648`
1. PDF 预览添加 📄 标识 + OCR 文字标题
2. 大文件预览截断 50k 字
3. 图片缩放控件 `[-][100%][+]`
4. Enter 键冲突修复（焦点在搜索框时不触发 openFile）
5. No results 引导：清空筛选 + 索引链接
6. 筛选持久化 localStorage
7. mtime 单位修复（`ts*1000` → `ts/1000`，后端微秒→前端 ms）全部 6 处
8. 侧边栏 File Types i18n
9. 搜索历史在输入时保留
10. 分页加页码输入跳转
11. 设置页自动保存，移除 Save 按钮

---

## 2026-07-31（第二轮：路径重构 + 迁移修复）

### 📁 相对路径存储
- **`843de19`** refactor: 文件路径由绝对→相对路径存储
  - `file_tracking` 和 Tantivy 索引 path 改为相对路径（相对 dir_config.path）
  - 新增 `to_relative()` / `to_absolute()` 辅助函数
  - 支持跨平台索引复用

### 🔧 修复
- **`8c66d08`** fix: LO 路径 onBlur 保存 + ScanDelta 真实 deleted/modified 值
- **`ead6023`** fix: batch 索引错误日志显示文件名+路径
- **`d599b64`** fix: 迁移数据后 data_dir 被设为消息字符串而非新路径
- **`0c65e66`** fix: 迁移数据完整修复（catch 缺失 + 允许空目录）
- **`e8d2ab2`** fix: get_stats 只统计活跃文件（`WHERE status='active'`）+ 绝对→相对路径自动迁移

---

## 2026-08-01（第三轮：扫尾 + 体验修复）

### 🔧 最后 5 项修复
- **`0c7f67f`** fix: `needs_reindex()` 抽取到 helpers.rs + ScanResult.added 分离 + list_dir_entries 过滤 deleted

### 📖 文档
- **`57dd72b`** docs: 基于项目现状全面重写 README 和用户手册

### 🚀 功能
- **`0ed36ae`** feat: 数据迁移后自动重启（`restart_app` 命令）
- **`19c595a`** feat: 设置页添加外部依赖面板（PaddleOCR/pdftoppm/LibreOffice 状态 + 一键复制安装命令）

### 🔧 修复
- **`6181000`** fix: 7个 TypeScript 编译错误
- **`eed560b`** fix: 迁移后改为确认对话框
- **`63d3d06`** fix: 索引状态页 Details 按钮无响应（`get_index_errors` 未注册 Tauri 命令）

---

## 2026-08-01（第四轮：更多 Bug + 文档 + 自动变更日志）

### 🔴 严重 Bug
- **`03949ac`** 修复 5 个 UX 缺陷
  - 删除文件无反应：`mark_deleted` SQL `WHERE path=?` 错误接收 UUID，改为 `WHERE id=?`
  - `.DS_Store` 被实时索引：`handle_event` watcher 回调遗漏 `is_excluded` 检查
  - 设置页安装命令显示三个平台：前端按 `navigator.platform` 过滤当前平台
  - LO 路径输入与依赖检测分离：合并到依赖面板同一行
  - 索引状态 `pending` 和 `errors` 关系不清：Pending 卡片加 `incl. errors` 副标题
- **`ae3857c`** 索引期间 UI 冻结：r2d2 连接池仅 8 个，Rayon 并行任务耗尽连接，前端 IPC 命令 `get()` 阻塞 → `max_size: 8→32` + `connection_timeout: 10s`
- **`8f8980c`** 启动扫描 VACUUM 阻塞：VACUUM 持有 SQLite 独占锁，移到 watcher 之后执行 + 发 `scan-completed` 事件

### 🟠 功能修复
- **`63d3d06`** Details 按钮无响应：`get_index_errors` 命令未注册为 Tauri handler，前端 `invoke` 静默失败
- **`0c65e66`** 迁移数据路径错误：`migrateData` 返回消息字符串，前端误当路径存 → 改 `selected` + 加 catch 弹窗
- **`0ed36ae`** 迁移后自动重启：新增 `restart_app` Tauri 命令 + 确认对话框
- **`19c595a`** 设置页外部依赖面板：PaddleOCR/pdftoppm/LibreOffice 状态 + 一键复制安装命令
- **`6181000`** 7 个 TS 编译错误：泛型类型错误 + 未使用导入 + API 签名变更

### 📖 文档
- **`57dd72b`** README + 用户手册全面重写
- **CHANGELOG.md** 首次创建（27 个 commit 完整记录）

### 🔧 工作流
- **`0adfab5`** 自动变更日志：Git post-commit hook 首次尝试 → 改为 AI 手动编写详细条目
- **`12a678b`** 添加 `AGENTS.md` 项目规范：变更记录规则、代码规范、关键文件索引

---

## 2026-08-01（第五轮：Browse 页重写为表格视图）

### 🚀 新功能
- **`a2e0e16`** Browse 页全面重写：从文件系统目录树浏览改为数据库驱动的表格视图
  - 新增后端 `list_files_db` 命令：分页查询 `file_tracking` 表，支持状态筛选（全部/已索引/未索引/失败）、文件类型筛选、文件名模糊搜索、多字段排序（名称/路径/类型/大小/时间）
  - 前端表格列：文件名（ellipsis 截断）| 路径（ellipsis + title 完整路径）| 类型 | 状态（✓/✗/○ 图标）
  - 工具栏：状态筛选下拉 + 类型筛选 + 搜索框 + 排序选择
  - URL `useSearchParams` 同步所有筛选状态，刷新/分享不丢失
  - 分页控件（上/下页 + 页码跳转）
  - 点击行 → 右侧预览面板（复用 PreviewPanel）
  - 移除旧的目录树递归逻辑和相关 state

### 🟠 IndexStatus 卡片跳转
- 索引状态页 StatCard 支持跳转：Total Files → Browse，Indexed → `?filter=indexed`，Pending → `?filter=pending`。OCR'd 跳全部（暂无对应筛选），Errors 保留展开详情功能

---

## 2026-08-01（第六轮：扫描流程 + 数据一致性修复）

### 🔴 严重 Bug
- **`b1ba768`** list_files_db SQL 参数错位：`where_clause` 的 `?` 占位符与 `LIMIT ? OFFSET ?` 位置冲突导致查询失败，Browse 页无内容 → 改用 `params_from_iter` 正确绑定；`sort=name` 改用 `path` 排序（file_name 不是 DB 列）
- **`b1ba768`** 删除目录后残留数据：`remove_dir` 只删 `dir_config` 行，file_tracking 孤儿记录（统计虚高）、Tantivy 文档（仍可搜索）、content_index 引用全部残留 → 增加清理：先按 dir_id 从 Tantivy 删文档，再硬删 `file_tracking` 行，最后 `cleanup_orphan_content` 清理孤儿 content

### 🚀 新功能
- **`ede3cce`** 扫描两阶段进度报告：`ScanProgress` 增加 `phase` 字段（`"scan"`/`"index"`），`batch_index` 增加进度回调，Phase 2 串行写入时每处理一个文件上报已索引数；三个扫描函数 walk 阶段发 `phase:"scan"`、索引阶段发 `phase:"index"`，前端状态栏和索引状态页据此显示"正在扫描/正在索引"

### 📖 文档
- **`1b06f2c`** 添加完整 CHANGELOG.md
- **`03684db`** 修复 CHANGELOG 格式

### 🏗️ 索引目录命名重构
- **索引目录撞车**：data_dir 名为 "index" 时与硬编码索引子目录 `data_dir/index` 撞车，产生双重 `index/index`。新增共享常量 `INDEX_DIR_NAME = ".ls-index"`，替换全部硬编码 `join("index")`（`lib.rs`/`cli.rs`/`commands/config.rs`/`commands/backup.rs`）。启动时检测旧布局 `data_dir/index` 并重命名为 `.ls-index`（幂等）。`phase: "index"` 扫描标记与 `data.db` 路径逻辑不受影响

---

## 2026-08-01

### 🏗️ 索引重建改为原子替换
- **重建中断不丢旧索引**：`rebuild_index`（`commands/index.rs`）不再先 `remove_dir_all` 删旧索引，改为：① 建临时目录 `index.tmp-<uuid>`（`uuid::Uuid::new_v4().simple()`，同父目录下 `with_file_name`）→ ② 清空 `file_tracking`/`content_index`（保留原逻辑）→ ③ 在 tmp 目录 `IndexManager::open_or_create` 并 swap 内存 → ④ `reset_writer` → ⑤ 全量扫描（逻辑不变，写入 tmp 索引）→ ⑥ `indexer.commit()` 确保落盘 → ⑦ 原子替换：旧目录 rename 为 `index.old`，tmp rename 为 `index_dir`，成功则删 backup，失败则回滚还原旧索引。所有错误退出路径清理 tmp_dir 并复位 `is_scanning`/`is_rebuilding`/`cancel_scan`。搜索已被 `is_rebuilding` 守卫，重建期间读旧索引不受影响

### 🚀 重建期间搜索守卫
- **重建索引时搜索返回友好错误**：`AppState` 新增 `is_rebuilding: Arc<AtomicBool>` 标志（`state.rs`），`rebuild_index` 启动时置 true、所有退出路径（含 spawn_blocking 内提前 return 与正常结束）置 false（`commands/index.rs`）；`search` 命令开头检查该标志，重建期间直接返回 `"索引重建中，请稍后再试"`（`commands/search.rs`）。`lib.rs` / `tests/ipc_test.rs` 的 `AppState::new` 调用点传入新参数。未改动 rebuild 的目录删除/重建逻辑（R1-3b 单独处理）

### 🔒 安全加固（2 项）
- **Tauri CSP 启用**：`tauri.conf.json` 中 `"csp": null` → 完整 CSP 策略（`default-src 'self'` + 白名单 script/style/img/media/connect/frame/font/worker）。`connect-src` 额外加入 `http://ipc.localhost` 以兼容 Windows/Linux 的 IPC 通道（macOS 走 `ipc://`），防止 IPC 被 CSP 阻断。前端仅本地资源，无远程内容受影响
- **fs 插件权限收窄 + scope**：`capabilities/default.json` 删除 `fs:allow-mkdir` / `fs:allow-remove` / `fs:allow-rename`（前端未使用）；保留读权限与 `fs:allow-write`（SearchPage.tsx 的 CSV 导出 `writeTextFile` 依赖它，且 `save()` 对话框会自动将选中路径加入 fs scope，导出不受影响）；新增 `fs.scope` 白名单（`$APPDATA` / `$APPLOCALDATA` / `$DOCUMENT` / `$DESKTOP` / `$DOWNLOAD` 递归）

### 🔴 Bug 修复
- **lock_writer 并发丢索引**：`lock_writer`（`indexer.rs`）先释放 writer 锁再创建 `IndexWriter`，两个线程并发首次写入时各建一个 writer，后写者覆盖前者、丢文档。改为全程持锁创建（`index_manager` 是 RwLock 读锁、不依赖 writer 锁，不会死锁）
- **切换语言清空 data_dir**：`setLang`（`i18n/index.tsx`）调用 `updateConfig({ data_dir: '', language: l })` 把配置里的 data_dir 清空，切换语言即丢全部数据。改为只传 `{ language: l }`；同时在 `updateConfig`（`api/config.ts`）加防呆，拒绝空 `data_dir` 并抛错 `data_dir cannot be empty`，从源头杜绝此类覆盖
- **上次取消后下次扫描立即被取消**：`cancel_scan` 标志在扫描开始时未复位，上次点过取消后，`trigger_scan`/`rebuild_index` 的循环第一次 `load` 就为 true 直接 break → 两个 `spawn_blocking` 闭包开头先 `cancel_scan.store(false, Ordering::Release)` 复位；循环内 `load(Relaxed)` 改为 `Acquire`，`cancel_scan` 命令 `store(true, ...)` 改为 `Release`，形成 acquire-release 同步对（`commands/index.rs`）
- **restore_backup 直接覆盖活跃 data.db 损坏数据库**：WAL 模式下连接池仍持有 data.db，`fs::copy` 覆盖与 WAL 冲突可能导致损坏。改为 SQLite 在线备份 API（`rusqlite::backup::Backup::new(&src, &mut dst)` + `step(-1)`，`step_to` 在 0.32 已改名；Busy/Locked 重试 3 次）从备份的 data.db 恢复到活跃连接，不再直接覆盖文件。索引目录改为 rebuild_index 的 tmp→rename 原子替换 + 切换 IndexManager。恢复完成 emit `restore-completed` 后自动重启生效。`AppState` 新增 `is_restoring: Arc<AtomicBool>` 防重入（`state.rs`/`lib.rs`/`tests/ipc_test.rs`，顺带修复 ipc_test 缺参编译错误）；`Cargo.toml` 启用 rusqlite `backup` feature（`commands/backup.rs`）

---

## 2026-08-01

### 🏗️ 临时目录 RAII 工具 TempDir
- **临时目录并发冲突与泄漏**：4 处用 `ls_*_{pid}` 命名系统临时目录的代码，并发/多实例运行时共享同一路径互相覆盖，且提前 return 时遗留垃圾目录。新增 `scanner/helpers.rs` 的 `TempDir`（`{prefix}_{pid}_{uuid}` 唯一路径 + Drop 自动 `remove_dir_all`），替换 4 处：
  - `commands/files.rs` `download_files`：`ls_download_{pid}` → `TempDir::new("ls_download")`（zip 打包）
  - `commands/search.rs` `export_search_results`：`ls_export_{pid}.{format}` → `TempDir::new("ls_export")`（CSV/文本导出）
  - `extractor/office/mod.rs` `extract_via_libreoffice`：`ls_lo_{pid}` → `TempDir::new("ls_lo")`（guard 留在函数作用域，路径 clone 进线程，移除手动 `remove_dir_all`）
  - `extractor/pdf.rs` `ocr_pdf_via_pdftoppm`：`ls_pdf_ocr_{pid}` → `TempDir::new("ls_pdf_ocr")`（移除手动 `remove_dir_all`）
  - 新增 2 个单测：drop 后目录被删除、路径唯一（`cargo test --lib scanner::helpers` 通过）

## 2026-08-01

### 🏗️ Tantivy path 字段改为相对路径
- **索引内 path 存绝对路径、与 DB 不一致**：`batch_index` 和 `index_file` 用 `file_path.to_string_lossy()`（绝对路径）写入 Tantivy 的 `path` 字段，而 `file_tracking.path` 存相对路径，搜索结果路径与 Browse 页不一致。修复：
  - `batch_index`：`ExtractedData.file_path_str` 改用 `job.rel_path`（`BatchJob.rel_path` 已由 scanner 传入，DB 也用它做 upsert）
  - `index_file`：调用方未传 rel_path 时，从 `dir_config::get_dir` 取目录根 + `helpers::to_relative` 补算相对路径；读文件内容仍用绝对路径（`file_path` 参数），仅写索引用相对路径（`indexer.rs`）
  - 存量绝对路径：无需单独迁移——`startup_scan` 每次启动会用 rel_path 重写索引；若搜索结果路径仍显示绝对路径，在索引状态页重建索引一次


## 统计

| 类别 | 数量 |
|------|:---:|
| 🔴 Bug 修复 | 30+ |
| 🏗️ 架构/性能改进 | 20 |
| 🎨 UI/UX 修复 | 25+ |
| 🚀 新功能 | 8 |
| 📖 文档 | 5 |
| **总计 commits** | **35** |
| **变更文件数** | **70+** |
