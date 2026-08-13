# 第十章：命令行

> CLI 搜索、扫描、监控、健康检查。

---

Link-Searcher 提供命令行接口，可与 GUI 共用同一数据目录。

## 步骤 1：搜索

```bash
link-searcher search "关键词"
# 别名
link-searcher index "关键词"
```

**示例**：
```bash
$ link-searcher search "预算"
2026年度预算表.xlsx (xlsx): 15.23
2026年Q3季度财务报表.xlsx (xlsx): 12.10
--- 2 results in 4ms ---
```

**参数**：

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `query` | 搜索关键词（必填） | — |
| `--limit` / `-l` | 最大返回结果数 | 10 |

## 步骤 2：扫描

```bash
# 扫描所有已配置的目录
link-searcher scan

# 扫描指定目录（自动注册到资料库）
link-searcher scan /path/to/documents
```

**示例**：
```bash
$ link-searcher scan ~/Documents/Link-Searcher-Demo

[scan] scanning /Users/xxx/Documents/Link-Searcher-Demo ...
[scan] scan 18/18
[scan] index 18/18
/Users/xxx/Documents/Link-Searcher-Demo: 18 files, 18 indexed (added 18, modified 0, deleted 0, errors 0) in 1465 ms
```

## 步骤 3：实时监控

```bash
link-searcher watch /path/to/documents
```

- 注册目录到资料库（如果尚未注册）
- 执行基线扫描
- 持续监控文件变更（新增、修改、删除）
- 按 `Ctrl-C` 退出

## 步骤 4：健康检查

```bash
link-searcher health
```

检查索引和数据库的健康状态：

```bash
$ link-searcher health
Link-Searcher index health check
  Data dir: /Users/xxx/Library/Application Support/link-searcher

  Index: OK
    Segments: 5
    Documents: 11666

  Database: OK
    Integrity check: ok
    Tracked files: 11671
    Indexed entries: 9207
```

**输出说明**：

| 信息 | 说明 |
|------|------|
| Data dir | 当前数据目录位置 |
| Index Segments | 索引段数（越多说明索引越碎片化） |
| Index Documents | 索引中的文档数 |
| Integrity check | 数据库完整性检查结果（应为 ok） |
| Tracked files | 数据库追踪的文件数 |
| Indexed entries | 已索引的内容条目数 |

## 注意事项

- CLI 与 GUI 共用同一数据目录
- **请勿同时运行扫描类命令**（CLI 和 GUI 同时扫描会触发写入锁竞争）
- 搜索命令可以随时运行，与 GUI 不冲突

## ✅ 本章验证清单

- [ ] 已运行搜索命令并看到结果
- [ ] 已运行扫描命令
- [ ] 已运行健康检查命令
- [ ] 已了解 CLI 与 GUI 共用数据目录的注意事项