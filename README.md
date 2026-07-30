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
- **Apple Vision** (macOS 内置) — 零安装，最佳性能
- **Windows OCR** (Windows 内置) — 零安装
- **Tesseract** (全平台) — 需安装
- **引擎测试** — 设置页面一键测试，未通过不允许索引

### 索引
- **实时监控** — 文件新增/修改/删除自动检测，增量更新
- **全量/增量扫描** — 按需选择
- **流式处理** — 百万级目录不卡顿
- **错误分类** — 访问拒绝/解析失败/OCR 失败/超时 分类记录
- **Fallback 链** — 直接提取 → OCR → 纯文本 → 跳过

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
- **全局快捷键** — Ctrl+Space 弹出搜索窗口
- **系统托盘** — 关闭窗口最小化到托盘
- **开机自启** — 可选
- **命令行搜索** — `link-searcher search "keyword"`
- **索引健康检查** — `link-searcher health`

## 快速开始

### 前提条件

- **Node.js** 20+：[下载](https://nodejs.org)
- **Rust** 1.85+：[安装](https://rustup.rs)

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
| OCR | Apple Vision / Windows OCR / Tesseract |
| 文件监控 | notify |
| 并行处理 | Rayon |
| 测试 | 95 测试 (80 unit + 9 integration + 6 IPC) |

## 项目结构

```
link-searcher/
├── src-tauri/           # Rust 后端
│   ├── src/
│   │   ├── main.rs      # 入口
│   │   ├── lib.rs        # Tauri 初始化
│   │   ├── cli.rs        # 命令行搜索
│   │   ├── search/       # Tantivy 搜索引擎
│   │   ├── db/           # SQLite 数据库
│   │   ├── extractor/    # 文本提取 + OCR
│   │   ├── scanner/      # 目录扫描 + 文件监控
│   │   ├── indexer.rs    # 索引服务
│   │   ├── commands/     # Tauri IPC 命令
│   │   └── state.rs      # 应用状态
│   └── tests/            # 集成测试
├── src/                  # React 前端
│   ├── api/              # IPC 调用封装
│   ├── components/       # 通用组件
│   ├── pages/            # 页面组件
│   └── hooks/            # 自定义 Hooks
├── USER_MANUAL.md        # 用户手册
└── TEST_PLAN.md          # 测试计划
```

## 测试

```bash
cd src-tauri && cargo test
```

## 许可证

MIT
