# Link-Searcher 变更日志

> 2026年7月30日 — 8月1日，共 27 个 commit，修复 55+ Bug，完成 20+ 功能改进

---

## 2026-07-30

### 项目初始化
- **ed1a639** Initial commit：Tauri 2 + React 19 + Tantivy 搜索引擎 + Tesseract OCR
- **874a0e4** chore：忽略 Tantivy 索引缓存文件

---

## 2026-07-31（第一轮：PaddleOCR + 启动流程 + Bug 修复）

### 🚀 PaddleOCR 内置引擎
| commit | 内容 |
|--------|------|
| `0e609c4` | **feat: PaddleOCR 内置引擎 + 启动扫描 + 实时监控** |
| | — 集成 `pure-onnx-ocr`（tract 纯 Rust ONNX 推理），PP-OCRv5 模型编译进二进制 |
| | — 引擎优先级：PaddleOCR(默认) → Apple Vision → Windows OCR → Tesseract |
| | — `include_bytes!` 内嵌 21MB 模型，零外部依赖 |
| | — 新增 `startup_scan()` 启动自动扫描 |
| | — 实时文件监控（notify 300ms 防抖） |
| | — 文件移位检测（MD5 哈希匹配） |
| | — 默认排除规则（`#` `$` `.` `~` 前缀文件 + `.tmp` `.bak` 后缀等） |
| | — 移除全局快捷键 Ctrl+Space |
| | — 更新 README + USER_MANUAL |

### 🔴 Bug 修复（12 项）
| commit | # | Bug | 修复 |
|--------|---|-----|------|
| `45db344` | 1 | `took_ms` 实为微秒 | `as_micros()` → `as_millis()` |
| | 2 | `mem::forget(watcher)` 线程泄漏 | watcher 存入 AppState |
| | 3 | MD5 哈希不一致（文件字节 vs 文本字节） | 统一使用文件字节 MD5 |
| | 4 | `upsert_file` ON CONFLICT 错误重置 `indexed=0` | SQL 加 CASE WHEN 条件 |
| | 5 | `last_scan` 秒 vs `mtime` 微秒精度不匹配 | `timestamp()` → `timestamp_micros()` |
| | 6 | CSV 导出 path 列写成 file_name | SearchHit 加 path 字段 |
| | 7 | OCR 引擎检查与 PaddleOCR 默认冲突 | 匹配区分各引擎 |
| | 8 | FileWatcher 只处理 paths[0] | 遍历所有 paths |
| | 10 | CSV 不转义特殊字符 | 所有列转义 |
| | 11 | `db_path.to_str().unwrap()` 非 ASCII 路径崩溃 | `to_string_lossy()` |
| | 12 | OCR 预处理临时文件 PID 并发冲突 | UUID 替代 PID |

### 🏗️ 架构改进（16 项）
| commit | 改进 |
|--------|------|
| `c898d07` | **refactor: 架构/性能/安全改进集** |
| | — 定期 commit（每 100 文件自动提交） |
| | — IndexReader 复用（缓存 + reload） |
| | — `content_suggest` 字段用于搜索建议 |
| | — `sort=name` Rust 侧排序 |
| | — `filename:` 正则解析（支持任意位置） |
| | — CLI data_dir 统一 |
| | — 移除非关键 unwrap/expect |
| | — PaddleOCR `Mutex + Send/Sync` 安全包装 |
| | — 取消扫描功能（`cancel_scan` AtomicBool） |
| | — 清理孤儿 content_index |
| | — 数据库 VACUUM |
| `59bb801` | **refactor: 流式MD5 + WalkDir 超时 + watcher 自动重连** |
| | — MD5 流式计算（BufReader 替代 read_to_end） |
| | — 文件大小上限 100MB，超大文件只读首尾 1MB |
| | — WalkDir 计数 3 秒超时保护 |
| | — FileWatcher 后台线程自动重连（3 次重试，500ms 间隔） |
| `75c7501` | **refactor: Rayon 并行索引** |
| | — `batch_index`：par_iter 并行提取 + 串行 Tantivy 写入 |

### 🎨 前端假功能修复（8 项）
| commit | # | 问题 | 修复 |
|--------|---|------|------|
| `73489ef` | 1 | 排序选择器"死控件" | 打通前端→API→后端 sort/sortOrder |
| | 2 | Pause/Resume 假按钮 | 改为取消扫描按钮 |
| | 3 | 文件类型分布假数据 | 新增 `get_file_type_stats` 命令 |
| | 4 | Recent Changes 计算错误 | 新增 `ScanDelta` 追踪真实数据 |
| | 5 | CSV 导出无保存对话框 | 系统 `save()` 对话框 |
| | 13 | DEBUG eprintln 遗留 | 删除 |

### 🟠 可用性改进（11 项）
| commit | # | 改进 |
|--------|---|------|
| `789a648` | 6 | PDF 预览添加 📄 标识 + OCR 文字标题 |
| | 7 | 大文件预览截断 50k 字 |
| | 8 | 图片缩放控件 `[-][100%][+]` |
| | 9 | Enter 键冲突修复（焦点在搜索框时不触发 openFile） |
| | 10 | No results 引导：清空筛选 + 索引链接 |
| | 11 | 筛选持久化 localStorage |
| | 12 | mtime 单位修复（`ts*1000` → `ts/1000`，后端微秒→前端 ms）全部 6 处 |
| | 14 | 侧边栏 File Types i18n |
| | 16 | 搜索历史在输入时保留 |
| | 17 | 分页加页码输入跳转 |
| | 19 | 设置页自动保存，移除 Save 按钮 |

---

## 2026-07-31（第二轮：路径重构 + 迁移修复）

### 📁 相对路径存储
| commit | 内容 |
|--------|------|
| `843de19` | **refactor: 文件路径由绝对→相对路径存储** |
| | — `file_tracking` 和 Tantivy 索引 path 改为相对路径（相对 dir_config.path） |
| | — 新增 `to_relative()` / `to_absolute()` 辅助函数 |
| | — 支持跨平台索引复用 |

### 🔧 修复
| commit | 内容 |
|--------|------|
| `8c66d08` | fix: LO 路径 onBlur 保存 + ScanDelta 真实 deleted/modified 值 |
| `ead6023` | fix: batch 索引错误日志显示文件名+路径 |
| `d599b64` | fix: 迁移数据后 data_dir 被设为消息字符串而非新路径 |
| `0c65e66` | fix: 迁移数据完整修复（catch 缺失 + 允许空目录） |
| `e8d2ab2` | fix: get_stats 只统计活跃文件（`WHERE status='active'`）+ 绝对→相对路径自动迁移 |

---

## 2026-08-01（第三轮：扫尾 + 体验修复）

### 🔧 最后 5 项修复
| commit | 内容 |
|--------|------|
| `0c7f67f` | fix: `needs_reindex()` 抽取到 helpers.rs + ScanResult.added 分离 + list_dir_entries 过滤 deleted |

### 📖 文档
| commit | 内容 |
|--------|------|
| `57dd72b` | **docs: 基于项目现状全面重写 README 和用户手册** |

### 🚀 功能
| commit | 内容 |
|--------|------|
| `0ed36ae` | feat: 数据迁移后自动重启（`restart_app` 命令） |
| `19c595a` | feat: 设置页添加外部依赖面板（PaddleOCR/pdftoppm/LibreOffice 状态 + 一键复制安装命令） |

### 🔧 修复
| commit | 内容 |
|--------|------|
| `6181000` | fix: 7个 TypeScript 编译错误 |
| `eed560b` | fix: 迁移后改为确认对话框 |
| `63d3d06` | fix: 索引状态页 Details 按钮无响应（`get_index_errors` 未注册 Tauri 命令） |
| `03949ac` | **fix: 5个 UX 缺陷修复** |
| | — 删除文件无反应：`mark_deleted` SQL `WHERE path`→`WHERE id` |
| | — `.DS_Store` 被索引：`handle_event` 加 `is_excluded` 检查 |
| | — 安装命令三平台全显示：按当前平台过滤 |
| | — LO 路径与依赖分离：合并到依赖面板同一行 |
| | — pending/errors 关系不清：Pending 卡片加 `incl. errors` 副标题 |
| `ae3857c` | fix: 索引期间 UI 冻结——数据库连接池 8→32 + 10s 超时 |
| `8f8980c` | fix: 启动扫描 VACUUM 后置 + 发送 `scan-completed` 事件 |

---

## 统计

| 类别 | 数量 |
|------|:---:|
| 🔴 Bug 修复 | 25+ |
| 🏗️ 架构/性能改进 | 20 |
| 🎨 UI/UX 修复 | 20+ |
| 🚀 新功能 | 6 |
| 📖 文档 | 3 |
| **总计 commits** | **31** |

---

## 2026-08-01（第五轮：Browse 页重写为表格视图）

### 🚀 新功能
- **Browse 页全面重写** (`a2e0e16`)：从文件系统目录树浏览改为数据库驱动的表格视图
  - 新增后端 `list_files_db` 命令：分页查询 `file_tracking` 表，支持状态筛选（全部/已索引/未索引/失败）、文件类型筛选、文件名模糊搜索、多字段排序（名称/路径/类型/大小/时间）
  - 前端表格列：文件名（ellipsis 截断）| 路径（ellipsis + title 完整路径）| 类型 | 状态（✓/✗/○ 图标）
  - 工具栏：状态筛选下拉 + 类型筛选 + 搜索框 + 排序选择
  - URL `useSearchParams` 同步所有筛选状态，刷新/分享不丢失
  - 分页控件（上/下页 + 页码跳转）
  - 点击行 → 右侧预览面板（复用 PreviewPanel）
  - 移除旧的目录树递归逻辑和相关 state

### 🟠 IndexStatus 卡片跳转
- **索引状态页 StatCard 支持跳转**：Total Files → Browse，Indexed → `?filter=indexed`，Pending → `?filter=pending`。OCR'd 跳全部（暂无对应筛选），Errors 保留展开详情功能

---

## 2026-08-01（第四轮：更多 Bug + 文档 + 自动变更日志）

### 🚀 新功能
- **扫描两阶段进度报告**：`ScanProgress` 增加 `phase` 字段（`"scan"`/`"index"`），`batch_index` 增加进度回调，Phase 2 串行写入时每处理一个文件上报已索引数；三个扫描函数（full/incremental/startup）walk 阶段发 `phase:"scan"`、索引阶段发 `phase:"index"`，前端状态栏和索引状态页据此显示"正在扫描/正在索引"（`scanner/mod.rs`、`indexer.rs`、`commands/index.rs`、`lib.rs`、`tests/integration.rs`、`api/index.ts`、`StatusBar.tsx`、`IndexStatus.tsx`）

### 🔴 严重 Bug
- **修复 5 个 UX 缺陷** (`03949ac`)
  - 删除文件无反应：`mark_deleted` SQL `WHERE path=?` 错误接收 UUID，改为 `WHERE id=?`
  - `.DS_Store` 被实时索引：`handle_event` watcher 回调遗漏 `is_excluded` 检查
  - 设置页安装命令显示三个平台：前端按 `navigator.platform` 过滤当前平台
  - LO 路径输入与依赖检测分离：合并到依赖面板同一行
  - 索引状态 `pending` 和 `errors` 关系不清：Pending 卡片加 `incl. errors` 副标题
- **索引期间 UI 冻结** (`ae3857c`)：r2d2 连接池仅 8 个，Rayon 并行任务耗尽连接，前端 IPC 命令 `get()` 阻塞 → `max_size: 8→32` + `connection_timeout: 10s`
- **启动扫描 VACUUM 阻塞** (`8f8980c`)：VACUUM 持有 SQLite 独占锁，移到 watcher 之后执行 + 发 `scan-completed` 事件
- **绝对→相对路径迁移遗漏**：存量 DB 记录未更新，全量删除 → 添加自动迁移函数

### 🟠 功能修复
- **Details 按钮无响应** (`63d3d06`)：`get_index_errors` 命令未注册为 Tauri handler，前端 `invoke` 静默失败
- **迁移数据路径错误** (`0c65e66`)：`migrateData` 返回消息字符串，前端误当路径存 → 改 `selected` + 加 catch 弹窗
- **迁移后自动重启** (`0ed36ae`)：新增 `restart_app` Tauri 命令 + 确认对话框
- **设置页外部依赖面板** (`19c595a`)：PaddleOCR/pdftoppm/LibreOffice 状态 + 一键复制安装命令
- **7 个 TS 编译错误** (`6181000`)：泛型类型错误 + 未使用导入 + API 签名变更

### 📖 文档
- **README + 用户手册全面重写** (`57dd72b`)：基于项目当前全部功能（PaddleOCR/启动扫描/监控/排除规则/相对路径等）
- **CHANGELOG.md**：27 个 commit 完整变更记录

### 🔧 工作流
- **自动变更日志** (`0adfab5`)：Git post-commit hook 首次尝试 → 改为 AI 手动编写详细条目
| 🚀 新功能 | 4（PaddleOCR 内置 / 启动扫描 / 外部依赖面板 / 自动重启） |
| 📖 文档 | 2（README + 用户手册重写） |
| 📁 重构 | 1（相对路径存储） |
| **总计 commits** | **27** |
| **变更文件数** | **60+** |

- **2026-08-01** 添加 `AGENTS.md` 项目规范：变更记录规则、代码规范、关键文件索引

### 🟠 功能修复
- **删除目录后残留数据**：`remove_dir` 只删 `dir_config` 行，file_tracking 孤儿记录（统计虚高）、Tantivy 文档（仍可搜索）、content_index 引用全部残留 → 增加清理：先按 dir_id 从 Tantivy 删文档，再硬删 `file_tracking` 行，最后 `cleanup_orphan_content` 清理孤儿 content（`commands/dirs.rs`）
