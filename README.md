# Link-Searcher

> 跨平台桌面全文搜索软件 —— 快速索引、搜索您的本地文档

[![Rust](https://img.shields.io/badge/Rust-1.85+-orange.svg)](https://www.rust-lang.org)
[![Tauri](https://img.shields.io/badge/Tauri-2-blue.svg)](https://tauri.app)
[![License](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

---

## 功能特性

### 搜索引擎

| 功能 | 说明 |
|------|------|
| **全文搜索** | Tantivy 引擎，BM25 相关性排序，毫秒级响应 |
| **中文分词** | jieba 分词，中英文混合搜索 |
| **模糊搜索** | 容错距离 1，自动纠正拼写错误 |
| **高级查询** | 通配符 `doc*`、短语 `"dog cat"`、`AND`/`OR`/`NOT` |
| **文件名搜索** | `filename:report.pdf` 语法，正则匹配任意位置 |
| **搜索建议** | 输入时自动补全，基于 content_suggest 字段前缀查询 |
| **排序** | 支持按相关性/日期/文件名/文件大小排序 |
| **分页** | 自适应窗口高度，动态调整每页行数 |
| **结果导出** | 一键导出为 CSV 文件 |

### AI 增强（可选）

在设置页「AI 服务」中**添加 AI Provider**（OpenAI 兼容网关：Ollama / OneAPI / vLLM / 各类中转均可，留空关闭），保存时自动拉取模型列表并按名字分类为 embedding / LLM，再从「当前使用」下拉框分别选择 embedding 与 LLM 模型：

| 功能 | 说明 |
|------|------|
| **语义搜索** | 搜索页「✦ 语义」开关——向量 embedding + BM25 结果**可调权重混合**（设置页「检索策略」滑杆，默认语义30%/关键词70%），搜"意思"而非仅"字面" |
| **文档摘要** | 预览面板「✦ AI 摘要」按钮，LLM 生成摘要并缓存 |
| **跨文件问答** | 浏览页多选文件后输入问题，AI 基于选中文件内容回答（RAG） |
| **AI 聊天** | 多轮对话式文档检索——首问自动检索、追问换范围；回答带「检索依据」溯源面板（BM25/语义/RRF 分数 + 改写标记）+「推理过程」时间线（查询改写 → 范围解析 → 检索 → 组装 → LLM 调用全链路追溯） |
| **检索范围控制** | 输入 `@` 弹出文件/目录选择器、左侧树状文件浏览器/右键文件或目录一键「加入检索范围」（跨轮累计，父目录自动吞并子路径，范围条 chips 可×删除）、`@文件` 编号引用 `[N]` 进材料、`/ext:` `/date:` `/范围:` 条件命令、「仅依据文档」严格模式（范围内无命中拒绝回答） |

> **隐私**：启用后文档文本会发送到所配置的网关。使用本地 Ollama（127.0.0.1）则内容不出本机。未配置时全部功能自动隐藏，搜索不受影响。
> **首次启用**：在设置页配置后，于索引页点击「✦ 补齐语义向量」（或跑一次扫描自动补）即可为已索引文件生成向量，**无需重新索引**。

### 支持的格式

| 格式 | 扩展名 | 提取方式 |
|------|--------|---------|
| PDF | `.pdf` | 文本提取 + 扫描件自动 OCR（pdftoppm 渲染 → PaddleOCR 识别） |
| Word | `.docx` `.doc` | `.doc` 经 rwml 纯 Rust 解析；`.docx` anydoc 原生解析 |
| Excel | `.xlsx` `.xls` | calamine 原生读取所有单元格 |
| PPT | `.pptx` `.ppt` | anydoc 原生解析 |
| 图片 | `.png` `.jpg` `.jpeg` `.gif` `.bmp` `.webp` `.tiff` | PaddleOCR 文字识别 |
| Markdown | `.md` | 直接读取 |
| 纯文本 | `.txt` `.csv` `.json` `.xml` `.yaml` `.toml` `.ini` `.log` | 直接读取 |
| 代码 | `.py` `.rs` `.ts` `.js` `.html` `.css` `.sql` `.sh` | 作为文本处理 |
| 未知 | 任意 | 纯文本回退尝试 |
| 压缩包 | `.zip` `.tar` `.tar.gz` `.tgz` `.tar.bz2` `.tbz2` `.tar.xz` `.txz` `.gz` `.bz2` `.xz` | 枚举条目，文本直接读，Office/PDF/图片走提取管线 |
| 音频 | `.mp3` `.wav` `.m4a` `.aac` `.flac` `.ogg` `.opus` `.wma` | FunASR-Nano 语音识别 + CAM++ 说话人分离，支持吴语/粤语/闽语 |

### OCR（文字识别）

**内置 PaddleOCR 引擎**，基于 PP-OCRv5 模型 + 纯 Rust ONNX 推理（tract），零 C/C++ 依赖。发布版**不内嵌模型**：首次启动的「依赖中心」向导会从国内镜像自动下载模型（约 20MB）到数据目录；开发版直接使用仓库内 `src-tauri/models/ppocrv5`。

| 引擎 | 状态 | 说明 |
|------|:---:|------|
| **PaddleOCR**（内置默认） | ✅ | PP-OCRv5 模型由首启向导/依赖中心镜像下载，支持中英文 |
| Apple Vision | ✅ | macOS 10.15+ 系统原生，ANE 硬件加速；macOS 默认引擎 |
| Windows OCR | ✅ | Windows 10+ 系统原生（语言包可用时为 Windows 默认引擎，避免调用外部 CLI） |
| Tesseract | ✅ | 备选引擎，需用户自行安装 `tesseract` CLI |

引擎优先级：平台原生（macOS Apple Vision / Windows OCR）→ PaddleOCR → Tesseract（自动降级）。配置的引擎在本机不可用时（如 Windows 上残留 macOS 默认值、PaddleOCR 模型未装）会自动回退到可用引擎。Windows 上所有外部 CLI 子进程（poppler/tesseract/ffmpeg）均以 `CREATE_NO_WINDOW` 隐藏运行，不会闪现黑色 cmd 窗口。

### 索引与监控

| 功能 | 说明 |
|------|------|
| **启动自动扫描** | 程序启动时自动对已配置目录执行增量扫描 |
| **实时文件监控** | 基于 notify，300ms 防抖，文件新增/修改/删除即时同步；目录配置变更自动触发增量扫描 |
| **索引会话日志** | 每次扫描独立日志文件（`logs/scan-{时间戳}.log`），日志页可查看历史会话 |
| **文件移位检测** | 通过 MD5 内容哈希 + 文件名/大小匹配，识别被移动的文件并更新路径（不重提取） |
| **全量扫描** | 两阶段处理（先计数后索引），超大目录 3 秒超时保护 |
| **增量扫描** | 仅处理 mtime 变化或未索引的文件 |
| **索引重建** | 清空索引和数据库，全部从头构建 |
| **批量并行索引** | Rayon `par_iter` 并行提取文本，Tantivy writer 串行写入 |
| **定期自动提交** | 每 100 个文件自动 commit，防止崩溃丢失全部进度 |
| **流式 MD5 计算** | BufReader 流式哈希，超大文件（>100MB）只读首尾 1MB |
| **内容去重** | 相同 MD5 的文件只提取一次，后续直接复用索引 |
| **扫描取消** | 支持中途取消扫描（`cancel_scan` 命令） |
| **错误分类** | 访问拒绝/解析失败/OCR 失败/超时，分别记录到数据库 |
| **数据备份** | 手动备份 / 增量备份链 / ZIP 导出(可选 AES-256 加密) / ZIP 恢复 / 死目录重映射 |

### 文件排除

扫描时自动排除以下类型（无需手动配置）：

| 规则 | 示例 |
|------|------|
| `#` 开头 | `#temp.md` `#backup#` |
| `$` 开头 | `$recycle` |
| `.` 开头（Unix 隐藏文件） | `.DS_Store` `.gitignore` `.env` |
| `~` 开头 | `~$temp.docx` |
| `.tmp` `.temp` `.bak` `.swp` `.swo` `~` 结尾 | `data.tmp` `backup.bak` |
| 精确名称 | `.DS_Store` `Thumbs.db` `.git` `.svn` `__pycache__` |

用户可在设置中添加额外 glob 排除规则。

### 跨平台路径

文件路径在数据库和索引中存储为**相对路径**（相对于监控目录根），而非绝对路径。两个完全同步的目录在不同操作系统上可共享同一份索引数据。

### 界面

| 功能 | 说明 |
|------|------|
| **暗色模式** | 浅色 / 深色 / 跟随系统，平滑过渡 |
| **多语言界面** | 中文 / English / 日本語 / 한국어，设置页切换 |
| **搜索页** | 搜索框 + 筛选面板 + 结果列表 + 预览面板（三栏布局） |
| **浏览页** | 表格视图，支持按索引状态筛选（全部/已索引/未索引/失败）、类型/搜索/排序、Cmd/Ctrl+单击或 Shift 区间多选、右键菜单（打开 / Finder 中显示 / 批量手动索引）、列宽拖拽 |
| **筛选面板** | 目录树筛选 + 文件扩展名筛选，支持持久化到 localStorage |
| **预览面板** | PDF 标识、OCR 文字、图片（缩放控件）、文本截断（5 万字符） |
| **键盘导航** | ↑↓ 选择结果，Enter 打开预览；焦点在搜索框时 Enter 提交搜索 |
| **分页** | 自适应表格高度动态分页，自动回第 1 页 |
| **No Results 引导** | 提供清空筛选、打开索引页等快捷操作 |
| **设置页** | 所有选项修改后自动保存，无需手动点击保存按钮 |
| **索引状态页** | 文件数/已索引/待处理/失败统计，Recent Changes（新增/修改/删除），文件类型分布 |
| **文件类型页** | 各类型依赖状态与文件数；「扫描过但不支持」区块展示白名单外扩展名（缺依赖格式可一键定位安装指引） |
| **日志查看** | 按类型筛选，支持关键字过滤，自动刷新 |
| **任务简报** | 长任务（验证索引/补齐向量/重提取）完成后，状态栏 📋 图标点亮显示摘要；点击跳转日志对应位置（自动暂停滚动） |

### 系统集成

| 功能 | 说明 |
|------|------|
| **关闭行为** | 关闭窗口直接退出程序（系统托盘已在 [路线图](CHANGELOG.md#路线图) 中规划） |
| **开机自启** | 可选（macOS LaunchAgent / Windows 启动项） |
| **命令行搜索** | `link-searcher search "keyword"`（别名 `index`） |
| **命令行扫描/监控** | `link-searcher scan [dir]` 扫描并退出；`link-searcher watch dir` 实时监控文件变更 |
| **索引健康检查** | `link-searcher health` |
| **数据迁移** | 设置页一键迁移索引和数据到新目录 |
| **数据备份** | 手动备份 / 自动定时备份 |

---

## 快速开始

### 前提条件

- **Node.js** 20+
- **Rust** 1.85+
- **音频识别（可选）**：ffmpeg，音频转写模型（FunASR-Nano ~850MB）在应用「依赖中心」下载（镜像优先）
- **Windows 额外**：VS Build Tools（MSVC + Windows SDK）。`cargo build` 需从 GitHub 拉 `tauri-plugin-mcp`（git 依赖）与 sherpa-onnx 预编译库——国内网络建议先跑 `scripts/setup-dev.ps1`（自动配 cargo/npm 镜像、预下载 sherpa 库），或设代理 `HTTPS_PROXY`

### 国内网络一键配置（推荐）

```bash
# macOS / Linux
./scripts/setup-dev.sh
# Windows（PowerShell）
.\scripts\setup-dev.ps1
```

脚本会写入 cargo 国内镜像（rsproxy.cn）、项目 `.npmrc`（npmmirror）、让 git 依赖复用系统代理；并自动检测安装可选系统依赖 poppler/ffmpeg（macOS 走 Homebrew、Debian/Ubuntu 走 apt、Windows 走 winget；`--skip-system-deps` / `-SkipSystemDeps` 跳过，`--include-tesseract` / `-IncludeTesseract` 加装 tesseract）。Windows 版额外把 sherpa-onnx 预编译库预下载到 `third_party/sherpa-onnx` 并设置 `SHERPA_ONNX_ARCHIVE_DIR`。

### 开发运行

```bash
git clone https://github.com/Gray-Chan-sh/link-searcher.git
cd link-searcher
./scripts/setup-dev.sh      # 可选：配镜像
npm ci
npm run tauri dev
```

> 开发版直接使用仓库内 `src-tauri/models/ppocrv5` 模型，OCR 零下载开箱可用。

### 构建

```bash
npm run tauri build
```

构建产物在 `src-tauri/target/release/bundle/`：
- **macOS** → `.dmg`
- **Windows** → `.msi` / `.exe`（NSIS）
- **Linux** → `.deb` / `.AppImage`

> **发布版模型不在安装包内**：为控制体积，PaddleOCR（~20MB）/BGE（~95MB）/FunASR（~850MB）等模型均不在包内。首次启动的「初始化依赖」向导按需从 GitHub Releases 经国内加速镜像链下载（ghfast.top → gh-proxy.com → GitHub 直连兜底）到数据目录（`~/Library/Application Support/link-searcher/models` 等），可跳过、可稍后在设置页「依赖中心」补装。

---

## 技术栈

| 层 | 技术 |
|-----|------|
| 桌面框架 | Tauri 2.x |
| 前端 | React 19 + TypeScript + Tailwind CSS 4 |

> 📚 **文档导航**
> - [用户手册](docs/USER_MANUAL.md) — 面向使用者的 12 章手册
> - [设计手册](docs/ARCHITECTURE.md) — 面向开发者，分模块讲解设计思想、架构与复现要点
> - [搜索 UX 实现](docs/SEARCH_UX_IMPLEMENTATION.md)
| 搜索引擎 | Tantivy 0.22 |
| 数据库 | SQLite（rusqlite + r2d2 连接池） |
| 中文分词 | jieba-rs |
| OCR | PaddleOCR PP-OCRv5 + tract（纯 Rust ONNX 推理） |
| 文本提取 | lopdf / calamine / quick-xml |
| 文件监控 | notify + notify-debouncer-full（300ms 防抖） |
| 并行处理 | Rayon |
| 图片处理 | image-rs + imageproc |

---

## 项目结构

```
link-searcher/
├── src-tauri/                 # Rust 后端
│   ├── src/
│   │   ├── main.rs            # 程序入口
│   │   ├── lib.rs             # Tauri 初始化 + 启动流程
│   │   ├── cli.rs             # 命令行接口
│   │   ├── config.rs          # 配置文件管理
│   │   ├── state.rs           # AppState（全局状态）
│   │   ├── indexer.rs          # 索引服务（batch_index、流式 MD5、自动commit）
│   │   ├── search/            # Tantivy 搜索引擎
│   │   │   ├── mod.rs         # IndexManager（reader 缓存）
│   │   │   ├── schema.rs      # 字段定义 + tokenizer 注册
│   │   │   ├── indexer.rs     # 文档增删
│   │   │   └── searcher.rs    # 搜索/建议/导出
│   │   ├── db/                # SQLite 数据库
│   │   │   ├── mod.rs         # 初始化 + 迁移 + VACUUM + 清理
│   │   │   ├── tracker.rs     # 文件追踪（CRUD + 统计 + 路径迁移）
│   │   │   └── dir_config.rs  # 目录配置
│   │   ├── extractor/         # 文本提取
│   │   │   ├── mod.rs         # 格式路由
│   │   │   ├── ocr.rs         # OCR 引擎调度
│   │   │   ├── paddleocr.rs   # PaddleOCR 集成（模型运行期定位 + 引擎池并行）
│   │   │   ├── pdf.rs         # PDF 提取 + pdftoppm OCR
│   │   │   ├── office/        # Office 提取（rwml/calamine/anydoc 原生，无外部依赖）
│   │   │   ├── image.rs       # 图片 OCR
│   │   │   └── text.rs        # 纯文本提取
│   │   ├── scanner/           # 目录扫描 + 文件监控
│   │   │   ├── mod.rs         # full/incremental/startup_scan + handle_event
│   │   │   ├── watcher.rs     # FileWatcher（后台线程 + 自动重连）
│   │   │   └── helpers.rs     # 排除规则 + 路径转换 + needs_reindex
│   │   └── commands/          # Tauri IPC 命令
│   │       ├── search.rs      # 搜索/建议/导出/文件类型统计
│   │       ├── index.rs       # 索引状态/扫描/重建/取消/健康检查
│   │       ├── files.rs       # 文件列表/预览/打开/浏览
│   │       ├── dirs.rs        # 目录管理/目录树
│   │       ├── config.rs      # 配置读写/数据迁移
│   │       ├── settings.rs    # 设置管理
│   │       ├── backup.rs      # 备份恢复
│   │       ├── tesseract.rs   # OCR 引擎管理
│   │       └── logs.rs        # 日志查看
│   ├── models/                # PaddleOCR ONNX 模型（PP-OCRv5）
│   ├── capabilities/          # Tauri 权限配置
│   └── tests/                 # 集成测试 + IPC 测试
├── src/                       # React 前端
│   ├── api/                   # IPC 调用封装（search / settings / files / config / index）
│   ├── components/            # 通用组件（SearchBar / FilterPanel / PreviewPanel / StatusBar / ResultList）
│   ├── pages/                 # 页面（Search / Browse / Directories / IndexStatus / Settings）
│   ├── hooks/                 # 自定义 Hook（useSearch / useDirs / useTheme）
│   └── i18n/                  # 国际化（en / zh）
├── assets/                    # 静态资源（字体等）
├── USER_MANUAL.md             # 用户手册
└── README.md                  # 本文件
```

---

## 测试

### 单元测试与集成测试

```bash
cd src-tauri && cargo test
```

当前测试：81 个（80 单元 + 9 集成 + 6 IPC），全部通过。

### 性能测试套件

```bash
# 生成测试数据（需要 Python 依赖：reportlab, python-docx, openpyxl, Pillow）
python3 scripts/gen_test_data.py /tmp/ls-test-1k 1000

# 运行性能测试（需先构建：npm run tauri build）
./scripts/perf_scan.sh /tmp/ls-test-1k 1k-files
```

`perf_scan.sh` 输出：文件数、数据大小、索引大小、DB 大小、内存峰值/均值、采样日志。
测试期间需在 GUI 中手动添加目录并触发全量扫描，完成后按 Enter 生成报告。

---

## 许可证

MIT
