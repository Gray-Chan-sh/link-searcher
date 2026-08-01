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
| **分页** | 每页 20 条，支持页码跳转 |
| **结果导出** | 一键导出为 CSV 文件 |

### 支持的格式

| 格式 | 扩展名 | 提取方式 |
|------|--------|---------|
| PDF | `.pdf` | 文本提取 + 扫描件自动 OCR（pdftoppm 渲染 → PaddleOCR 识别） |
| Word | `.docx` `.doc` | XML 解析提取；旧版 `.doc` 通过 LibreOffice 提取 |
| Excel | `.xlsx` `.xls` | calamine 读取所有单元格 |
| PPT | `.pptx` `.ppt` | XML 解析幻灯片文本 |
| 图片 | `.png` `.jpg` `.jpeg` `.gif` `.bmp` `.webp` `.tiff` | PaddleOCR 文字识别 |
| Markdown | `.md` | 直接读取 |
| 纯文本 | `.txt` `.csv` `.json` `.xml` `.yaml` `.toml` `.ini` `.log` | 直接读取 |
| 代码 | `.py` `.rs` `.ts` `.js` `.html` `.css` `.sql` `.sh` | 作为文本处理 |
| 未知 | 任意 | 纯文本回退尝试 |

### OCR（文字识别）

**内置 PaddleOCR 引擎**，基于 PP-OCRv5 模型 + 纯 Rust ONNX 推理（tract），零 C/C++ 依赖，无需用户安装任何额外软件。

| 引擎 | 状态 | 说明 |
|------|:---:|------|
| **PaddleOCR**（默认） | ✅ | 内置引擎，模型编译进二进制（约 21MB），支持中英文，开箱即用 |
| Apple Vision | ⏳ | macOS 10.15+ 系统原生，计划后续实现 |
| Windows OCR | ⏳ | Windows 10+ 系统原生，计划后续实现 |
| Tesseract | ✅ | 备选引擎，需用户自行安装 `tesseract` CLI |

引擎优先级：PaddleOCR → Apple Vision → Windows OCR → Tesseract（自动降级）。

### 索引与监控

| 功能 | 说明 |
|------|------|
| **启动自动扫描** | 程序启动时自动对已配置目录执行增量扫描 |
| **实时文件监控** | 基于 notify，300ms 防抖，文件新增/修改/删除即时同步 |
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
| **搜索页** | 搜索框 + 筛选面板 + 结果列表 + 预览面板（三栏布局） |
| **浏览页** | 目录树 + 文件列表 + 预览面板，支持按索引状态筛选（全部/已索引/未索引/失败） |
| **筛选面板** | 目录树筛选 + 文件扩展名筛选，支持持久化到 localStorage |
| **预览面板** | PDF 标识、OCR 文字、图片（缩放控件）、文本截断（5 万字符） |
| **键盘导航** | ↑↓ 选择结果，Enter 打开预览；焦点在搜索框时 Enter 提交搜索 |
| **分页** | 上一页/下一页 + 页码输入跳转 |
| **No Results 引导** | 提供清空筛选、打开索引页等快捷操作 |
| **设置页** | 所有选项修改后自动保存，无需手动点击保存按钮 |
| **索引状态页** | 文件数/已索引/待处理/失败统计，Recent Changes（新增/修改/删除），文件类型分布 |
| **日志查看** | 按类型筛选，自动刷新 |

### 系统集成

| 功能 | 说明 |
|------|------|
| **系统托盘** | 关闭窗口最小化到托盘，后台持续运行 |
| **开机自启** | 可选（macOS LaunchAgent / Windows 启动项） |
| **命令行搜索** | `link-searcher search "keyword"` |
| **索引健康检查** | `link-searcher health` |
| **数据迁移** | 设置页一键迁移索引和数据到新目录 |
| **数据备份** | 手动备份 / 自动定时备份 |

---

## 快速开始

### 前提条件

- **Node.js** 20+
- **Rust** 1.85+

> PaddleOCR 引擎已内置，无需额外安装 OCR 软件。如需备选 Tesseract 引擎或 LibreOffice（旧版 `.doc` `.xls` `.ppt`），请参考用户手册。

### 开发运行

```bash
git clone https://github.com/Gray-Chan-sh/link-searcher.git
cd link-searcher
npm install
npm run tauri dev
```

### 构建

```bash
npm run tauri build
```

构建产物在 `src-tauri/target/release/bundle/`：
- **macOS** → `.dmg`
- **Windows** → `.msi`
- **Linux** → `.deb` / `.AppImage`

---

## 技术栈

| 层 | 技术 |
|-----|------|
| 桌面框架 | Tauri 2.x |
| 前端 | React 19 + TypeScript + Tailwind CSS 4 |
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
│   │   │   ├── paddleocr.rs   # PaddleOCR 集成（模型内嵌 + Mutex 安全包装）
│   │   │   ├── pdf.rs         # PDF 提取 + pdftoppm OCR
│   │   │   ├── office/        # Office 提取 + LibreOffice 集成
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
