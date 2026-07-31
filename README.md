# Link-Searcher

> 跨平台桌面全文搜索软件 — 快速索引、搜索您的本地文档

[![Rust](https://img.shields.io/badge/Rust-1.85+-orange.svg)](https://www.rust-lang.org)
[![Tauri](https://img.shields.io/badge/Tauri-2-blue.svg)](https://tauri.app)
[![License](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

## 截图

(待添加)

## 功能特性

### 搜索
- **全文搜索** — Tantivy 搜索引擎，BM25 相关性排序，毫秒级响应
- **中文分词** — 集成 jieba 分词，支持中英文混合搜索
- **模糊搜索** — 自动纠正拼写错误，容错距离 1
- **查询语法** — 通配符 `doc*`、短语 `"dog cat"`、布尔运算 `AND`/`OR`/`NOT`
- **文件名搜索** — `filename:report.pdf` 语法
- **搜索建议** — 输入时自动补全
- **目录树筛选** — 展开任意层级子目录，选中特定范围搜索
- **搜索结果高亮** — 预览面板中搜索词高亮显示

### 文件格式
| 格式 | 支持 |
|------|------|
| PDF | ✅ 文本提取 + 扫描件 OCR |
| Word (.docx) | ✅ 段落提取 |
| Excel (.xlsx) | ✅ 单元格提取 |
| PPT (.pptx) | ✅ 幻灯片提取 |
| 纯文本 (.txt, .md, .csv, .json, .xml, .yaml, .toml, .ini, .log) | ✅ |
| 代码 (.py, .rs, .ts, .js, .html, .css, .sql, .sh, .bat) | ✅ |
| 图片 (.png, .jpg, .gif, .bmp, .webp, .tiff) | ✅ OCR 识别 |
| 未知格式 | ✅ 纯文本回退尝试 |

### OCR 引擎

Link-Searcher 内置 **PaddleOCR** 引擎（基于 PP-OCRv5 模型，纯 Rust ONNX 推理），无需用户安装任何外部依赖即可识别中英文图片文字。

| 引擎 | 状态 | 说明 |
|------|------|------|
| **PaddleOCR**（默认） | ✅ 内置 | 零安装，纯 Rust 实现，支持中英文。模型编译进二进制，开箱即用 |
| Apple Vision | ⏳ 待实现 | macOS 10.15+ 内置，计划后续支持 |
| Windows OCR | ⏳ 待实现 | Windows 10+ 内置，计划后续支持 |
| Tesseract | ✅ 备选 | 需用户自行安装，作为后备引擎 |

引擎优先级：PaddleOCR → Apple Vision → Windows OCR → Tesseract（自动降级）

### 索引
- **启动自动扫描** — 程序启动时自动对已配置目录进行增量扫描
- **实时文件监控** — 启动后对已配置目录进行实时监控，新增/修改/删除自动检测、增量更新
- **文件移位检测** — 通过 MD5 内容哈希识别被移动的文件，更新路径而不重新索引
- **全量/增量扫描** — 按需选择触发
- **流式处理** — 百万级目录不卡顿
- **错误分类** — 访问拒绝/解析失败/OCR 失败/超时 分类记录
- **Fallback 链** — 直接提取 → OCR → 纯文本 → 跳过
- **内容去重** — 相同内容的文件只提取一次，通过 MD5 复用

### 文件排除

扫描时自动排除以下类型文件（无需手动配置）：

| 规则 | 示例 |
|------|------|
| 以 `#` 开头 | `#temp.md`, `#backup#` |
| 以 `$` 开头 | `$recycle` |
| 以 `.` 开头（Unix 隐藏文件） | `.DS_Store`, `.gitignore` |
| 以 `~` 开头 | `~$temp.docx` |
| 以 `.tmp/.temp/.bak/.swp/.swo/~` 结尾 | `data.tmp`, `backup.bak` |
| 精确名称 | `.DS_Store`, `Thumbs.db`, `.git`, `.svn`, `__pycache__` |

用户可在设置中添加额外的 glob 排除规则。

### 界面
- **暗色模式** — 浅色/深色/跟随系统，平滑过渡动画
- **虚拟滚动** — 万级结果不卡顿
- **键盘导航** — 方向键上下切换，Enter 打开
- **右键菜单** — 打开/复制路径/在文件夹中显示
- **拖拽添加目录** — 从文件管理器拖入
- **预览面板** — 可拖拽宽度，搜索词高亮，匹配导航
- **图片预览** — 直接显示图片文件
- **日志查看** — 按类型筛选，自动刷新
- **首次使用引导** — 三步向导

### 系统集成
- **系统托盘** — 关闭窗口最小化到托盘
- **开机自启** — 可选
- **命令行搜索** — `link-searcher search "keyword"`
- **索引健康检查** — `link-searcher health`

## 快速开始

### 前提条件

- **Node.js** 20+：[下载](https://nodejs.org)
- **Rust** 1.85+：[安装](https://rustup.rs)

> **注意**：PaddleOCR 引擎已内置于二进制中，无需额外安装 OCR 软件。如需备选 Tesseract 引擎，请自行安装。

### 安装与运行

```bash
# 克隆项目
git clone <repo-url> link-searcher
cd link-searcher

# 安装前端依赖
npm install

# 启动开发模式
npm run tauri dev
```

### 构建发布包

```bash
npm run tauri build
```

构建产物在 `src-tauri/target/release/bundle/`：
- **macOS** → `.dmg`
- **Windows** → `.msi`
- **Linux** → `.deb` / `.AppImage`

## 技术栈

| 层 | 技术 |
|-----|------|
| 桌面框架 | Tauri 2.x |
| 前端 | React 19 + TypeScript + Tailwind CSS 4 |
| 搜索引擎 | Tantivy 0.22 (Rust) |
| 数据库 | SQLite (rusqlite + r2d2) |
| 中文分词 | jieba-rs |
| 文本提取 | lopdf / calamine / quick-xml |
| OCR | PaddleOCR（内置，PP-OCRv5 + tract ONNX 推理） |
| 文件监控 | notify + notify-debouncer-full |
| ONNX 推理 | tract（纯 Rust，零 C/C++ 依赖） |
| 并行处理 | Rayon |
| 测试 | 81 测试 (80 unit + 9 integration + 6 IPC) |

## 项目结构

```
link-searcher/
├── src-tauri/              # Rust 后端
│   ├── src/
│   │   ├── main.rs         # 入口
│   │   ├── lib.rs          # Tauri 初始化 + 启动扫描
│   │   ├── cli.rs           # 命令行搜索
│   │   ├── search/          # Tantivy 搜索引擎
│   │   ├── db/              # SQLite 数据库
│   │   ├── extractor/       # 文本提取 + OCR
│   │   │   ├── ocr.rs       # OCR 引擎调度
│   │   │   ├── paddleocr.rs # PaddleOCR 集成（模型内嵌）
│   │   │   ├── pdf.rs       # PDF 提取 + OCR
│   │   │   ├── office/      # Office 文档提取
│   │   │   ├── image.rs      # 图片 OCR
│   │   │   └── text.rs      # 纯文本提取
│   │   ├── scanner/         # 目录扫描 + 文件监控
│   │   │   ├── mod.rs       # Scanner：全量/增量/启动扫描
│   │   │   ├── watcher.rs   # FileWatcher：实时文件监控
│   │   │   └── helpers.rs   # 扫描辅助 + 排除规则
│   │   ├── indexer.rs       # 索引服务
│   │   ├── commands/        # Tauri IPC 命令
│   │   └── state.rs         # 应用状态
│   ├── models/              # PaddleOCR ONNX 模型文件
│   └── tests/               # 集成测试
├── src/                     # React 前端
│   ├── api/                 # IPC 调用封装
│   ├── components/          # 通用组件
│   ├── pages/               # 页面组件
│   └── hooks/               # 自定义 Hooks
├── USER_MANUAL.md           # 用户手册
└── TEST_PLAN.md             # 测试计划
```

## 测试

```bash
cd src-tauri && cargo test
```

## 许可证

MIT
