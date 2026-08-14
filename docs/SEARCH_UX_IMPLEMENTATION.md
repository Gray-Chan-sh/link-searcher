# Link-Searcher 搜索 UX 实施手册

> 目标：单人 + 10k+ 中文文档 + 全文检索核心
> 时间线：第一波 ~1-2 天，第二波 ~1 周，第三波 ~2 周

---

## 目录

- [第一波：接现有能力（最高 ROI）](#第一波接现有能力最高-roi)
- [第二波：核心增量](#第二波核心增量)
- [第三波：中文增强 + 打磨](#第三波中文增强--打磨)
- [附录：键盘快捷键手册](#附录键盘快捷键手册)

---

# 第一波：接现有能力（最高 ROI）

> 后端已实现，前端从未接。总工作量 ~1-2 天。

---

## 1.1 SearchBar 接搜索建议下拉

### 现状

`useSearch` 已暴露 `suggestions`、`fetchSuggestions`、`clearSuggestions`，`SearchBar` 从未接收这些 props。`suggest()` 后端已实现（Tantivy `content_suggest` 字段前缀查询）。

### 修改

**文件：`src/components/SearchBar.tsx`**

```tsx
interface SearchBarProps {
  query: string
  loading: boolean
  onQueryChange: (q: string) => void
  onSubmit: () => void
  // 新增：
  suggestions: string[]
  onFetchSuggestions: (prefix: string) => void
  onClearSuggestions: () => void
  onSelectSuggestion: (suggestion: string) => void
}
```

**UI 行为：**

- 输入时（200ms 防抖，`useSearch` 已实现）→ 调用 `onFetchSuggestions`
- 下拉列表出现在搜索框下方，最多 10 条
- `↑/↓` 导航建议列表，`Enter` 选中并提交搜索
- `Esc` 关闭建议列表
- 点击建议项 → 填入搜索框并提交
- 失焦时关闭（`onBlur` + 延迟 `onClearSuggestions`）

**样式参考：** 灰色背景圆角卡片，`z-50`，与搜索框同宽，每项显示建议文本 + 搜索图标。

**文件：`src/pages/SearchPage.tsx`**

```tsx
// 传递新 props 给 SearchBar
<SearchBar
  query={search.query}
  loading={search.status === 'loading'}
  onQueryChange={search.setQuery}
  onSubmit={search.submitSearch}
  suggestions={search.suggestions}
  onFetchSuggestions={search.fetchSuggestions}
  onClearSuggestions={search.clearSuggestions}
  onSelectSuggestion={(s) => {
    search.setQuery(s)
    search.submitSearch()
  }}
/>
```

### 验证

1. 输入 "合" → 200ms 后出现下拉，显示 "合同"、"合作协议" 等
2. `↓` 选中建议项 → `Enter` 提交搜索
3. 点击外部区域 → 建议列表关闭
4. 输入为空 → 无建议（不干扰历史记录显示）

---

## 1.2 搜索历史空状态 + 置顶 UI

### 现状

后端 `search_history.rs` 自动记录每次搜索，支持 `pin_entry`/`delete_entry`/`clear_history`，`getSearchHistory` API 存在。**前端从未渲染。**

### 修改

**文件：`src/components/SearchBar.tsx`**

新增 `SearchHistoryDropdown` 组件（或内联在 SearchBar 中）：

```tsx
interface SearchHistoryEntry {
  id: string
  query: string
  result_count: number
  pinned: boolean
  created_at: number
}
```

**UI 行为：**

- 搜索框为空且聚焦时 → 显示历史下拉
- 历史条目按：置顶 > 最近 > 更早 排序
- 每条显示：查询文本 + 结果数 + 日期（相对时间，如"3 分钟前"）
- 置顶条目显示 📌 图标
- 每行右侧 hover 时显示：置顶/取消置顶、删除（单条）
- 底部操作：清空历史
- `↑/↓` 导航历史列表，`Enter` 重跑该搜索
- `Delete` 键删除选中的历史条目

**文件：`src/pages/SearchPage.tsx`**

```tsx
// 新增状态
const [searchHistory, setSearchHistory] = useState<SearchHistoryEntry[]>([])
const [showHistory, setShowHistory] = useState(false)

// 加载历史
useEffect(() => {
  getSearchHistory().then(setSearchHistory).catch(() => {})
}, [])
```

**文件：`src/api/search.ts`**

已有 `getSearchHistory`、`clearSearchHistory`，无需新增。

可能需要新增 `pinSearchHistory` / `deleteSearchHistoryEntry` 的 API 调用封装（后端已有 `pin_entry` / `delete_entry`，缺少前端 API 封装）。

### 验证

1. 搜索框聚焦且为空 → 显示最近 10 条搜索历史
2. 置顶一条 → 刷新后置顶条目始终在顶部
3. 删除单条 → 列表刷新
4. 清空全部 → 列表为空
5. `Alt+↑/↓` 浏览历史（参考 Everything）

---

## 1.3 模糊搜索 + 日期范围 UI 暴露

### 现状

后端 `SearchParams` 已支持 `fuzzy`、`date_from`、`date_to`，前端 `search()` 参数已传递，但 UI 从无暴露。

### 修改

**文件：`src/pages/SearchPage.tsx`**

在排序下拉旁新增两个控件：

**模糊搜索开关：**

```tsx
<label className="flex items-center gap-1.5 text-xs text-gray-500">
  <input
    type="checkbox"
    checked={search.fuzzy}
    onChange={(e) => search.setFuzzy(e.target.checked)}
    className="rounded"
  />
  模糊
</label>
```

**文件：`src/hooks/useSearch.ts`**

```tsx
// 新增状态
const [fuzzy, setFuzzy] = useState(false)
const [dateFrom, setDateFrom] = useState<string | null>(null)
const [dateTo, setDateTo] = useState<string | null>(null)

// 新增方法
const setFuzzy = useCallback((v: boolean) => {
  setFuzzyState(v)
  executeSearch(...)
}, [])

// 修改 executeSearch 传递 fuzzy / date_from / date_to
```

**日期范围：**

```tsx
// 轻量级日期选择器（无需第三方库，两个原生 <input type="date">）
<input
  type="date"
  value={search.dateFrom ?? ''}
  onChange={(e) => search.setDateFrom(e.target.value || null)}
  className="text-xs rounded border px-1 py-0.5"
/>
<span className="text-xs text-gray-400">~</span>
<input
  type="date"
  value={search.dateTo ?? ''}
  onChange={(e) => search.setDateTo(e.target.value || null)}
  className="text-xs rounded border px-1 py-0.5"
/>
```

**布局参考：** 放在排序下拉右侧，与语义切换同行，紧凑排列。

### 验证

1. 开启模糊 → 搜 "docment" 匹配 "document"
2. 设置日期范围 → 仅返回该日期范围内的文件
3. 切换模糊时自动重搜
4. 日期范围为空时回退到不过滤

---

# 第二波：核心增量

> ~1 周

---

## 2.1 搜索历史带上下文（点击追踪）

### 目标

搜索历史不只是记查询字符串，还要记录"点了哪个结果"——帮助用户快速恢复上次的搜索上下文。

### 数据模型

**文件：`src-tauri/src/db/search_history.rs`**

```sql
-- 现有 search_history 表扩展
ALTER TABLE search_history ADD COLUMN clicked_results TEXT;  -- JSON array: ["file_id_1", "file_id_2"]
ALTER TABLE search_history ADD COLUMN sort_field TEXT;
ALTER TABLE search_history ADD COLUMN filters TEXT;           -- JSON: { "dir_ids": [], "ext_filter": [], "fuzzy": false }
```

或新建 `search_history_click` 表：

```sql
CREATE TABLE search_history_clicks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  history_id INTEGER NOT NULL REFERENCES search_history(id) ON DELETE CASCADE,
  file_id TEXT NOT NULL,
  clicked_at INTEGER NOT NULL DEFAULT (unixepoch()),
  UNIQUE(history_id, file_id)
);
```

### 后端修改

**文件：`src-tauri/src/commands/search.rs`**

```rust
// 新增命令
#[tauri::command]
fn record_search_click(history_id: i64, file_id: String) -> Result<()> {
    // 插入 search_history_clicks
}

// 修改 get_search_history 返回点击记录
#[tauri::command]
fn get_search_history() -> Result<Vec<SearchHistoryWithClick>> {
    // JOIN search_history_clicks, 按 history_id 分组
}
```

### 前端修改

**文件：`src/api/search.ts`**

```tsx
export async function recordSearchClick(historyId: number, fileId: string): Promise<void> {
  return invoke('record_search_click', { historyId, fileId })
}

// SearchHistoryEntry 扩展
export interface SearchHistoryEntry {
  id: string
  query: string
  result_count: number
  pinned: boolean
  created_at: number
  clicked_files?: string[]  // 新增
  sort_field?: string
  filters?: string
}
```

**文件：`src/components/SearchBar.tsx`**

历史下拉中每条展示增强：

```
📌 "合同 供应商"       → 42 条结果，3 分钟前
   已打开：合同-A.pdf、合同-B.pdf
```

### 文件：`src/pages/SearchPage.tsx`

在 `onSelect` 回调中记录点击：

```tsx
const handleSelectHit = (hit: SearchHit) => {
  recordSearchClick(currentHistoryId, hit.file_id)
  setSelectedHit(hit)
}
```

### 验证

1. 搜 "合同" → 打开 2 个文件 → 查看历史 → 显示已打开的文件名
2. 再次打开相同搜索 → 历史中显示上次打开的文件
3. 清空历史 → 点击记录一并清除

---

## 2.2 保存搜索 = 智能文件夹（Smart Groups）

### 目标

将当前搜索条件（查询 + 筛选 + 排序 + 语义）保存为具名的虚拟文件夹，在侧边栏显示，点击即重跑。

### 数据模型

**文件：`src-tauri/src/db/mod.rs`**（新增表）

```sql
CREATE TABLE saved_searches (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  query TEXT NOT NULL,
  dir_ids TEXT,              -- JSON array
  ext_filter TEXT,           -- JSON array
  sort_field TEXT,
  sort_order TEXT,
  semantic INTEGER DEFAULT 0,
  fuzzy INTEGER DEFAULT 0,
  date_from TEXT,
  date_to TEXT,
  icon TEXT DEFAULT 'search',  -- 图标名称
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
```

### 后端

**文件：`src-tauri/src/commands/search.rs`**

```rust
#[tauri::command]
fn list_saved_searches() -> Result<Vec<SavedSearch>> { ... }

#[tauri::command]
fn save_search(params: SaveSearchParams) -> Result<SavedSearch> { ... }

#[tauri::command]
fn delete_saved_search(id: i64) -> Result<()> { ... }

#[tauri::command]
fn rename_saved_search(id: i64, name: String) -> Result<()> { ... }
```

**文件：`src-tauri/src/commands/search.rs`** 或新建 `src-tauri/src/commands/saved_searches.rs`

### 前端

**文件：`src/api/search.ts`**

```tsx
export interface SavedSearch {
  id: number
  name: string
  query: string
  dir_ids?: string[]
  ext_filter?: string[]
  sort_field?: string
  sort_order?: string
  semantic?: boolean
  fuzzy?: boolean
  date_from?: string
  date_to?: string
  created_at: number
  updated_at: number
}

export async function saveSearch(params: SaveSearchParams): Promise<SavedSearch> { ... }
export async function listSavedSearches(): Promise<SavedSearch[]> { ... }
export async function deleteSavedSearch(id: number): Promise<void> { ... }
export async function renameSavedSearch(id: number, name: string): Promise<void> { ... }
```

**文件：`src/pages/SearchPage.tsx`**

搜索栏右侧新增「保存搜索」按钮：

```tsx
<button onClick={() => setShowSaveDialog(true)} className="...">
  💾 保存搜索
</button>
```

保存对话框：
- 默认名称自动生成（如 "合同 - 2025-03-21"）
- 可编辑名称
- 保存后出现在侧边栏

**文件：`src/App.tsx`** 或侧边栏组件

侧边栏新增「已保存搜索」区域：

```
┌──────────────────┐
│ 已保存搜索        │
│ 🔍 2025年合同     │
│ 🔍 技术文档      │
│ 🔍 项目 Alpha    │
│   [+] 新建       │
└──────────────────┘
```

点击已保存搜索项 → 跳转至搜索页并自动填入条件、执行搜索。

### 验证

1. 搜索后点击「保存搜索」→ 输入名称 → 保存成功
2. 侧边栏显示已保存的搜索
3. 点击侧边栏项 → 自动执行搜索
4. 删除/重命名已保存搜索
5. 重启应用后已保存搜索依然存在

---

## 2.3 文件标签（Tags）

### 目标

给文件打标签，支持 `tag:` 前缀搜索，标签存储在 SQLite（不动原文件）。

### 数据模型

**文件：`src-tauri/src/db/mod.rs`**

```sql
CREATE TABLE tags (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  color TEXT,              -- 可选颜色
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE file_tags (
  file_id TEXT NOT NULL REFERENCES file_tracking(id) ON DELETE CASCADE,
  tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  PRIMARY KEY (file_id, tag_id)
);
```

### 后端

**文件：`src-tauri/src/commands/files.rs`** 或新建 `src-tauri/src/commands/tags.rs`

```rust
#[tauri::command]
fn list_tags() -> Result<Vec<Tag>> { ... }

#[tauri::command]
fn add_tag(name: String, color: Option<String>) -> Result<Tag> { ... }

#[tauri::command]
fn delete_tag(id: i64) -> Result<()> { ... }

#[tauri::command]
fn tag_file(file_id: String, tag_id: i64) -> Result<()> { ... }

#[tauri::command]
fn untag_file(file_id: String, tag_id: i64) -> Result<()> { ... }

#[tauri::command]
fn get_file_tags(file_id: String) -> Result<Vec<Tag>> { ... }

#[tauri::command]
fn search_by_tag(tag_name: String) -> Result<Vec<String>> { ... }  // 返回 file_id 列表
```

### 搜索集成

**文件：`src-tauri/src/commands/search.rs`**

`search` 命令处理 `tag:` 前缀参数：

```rust
// 解析 query 中的 tag:xxx
// 查 file_tags JOIN tags WHERE tags.name = xxx
// 将匹配的 file_ids 作为过滤条件传入搜索
```

### 前端

**文件：`src/api/files.ts`**

```tsx
export interface Tag {
  id: number
  name: string
  color?: string
}

export async function listTags(): Promise<Tag[]> { ... }
export async function addTag(name: string, color?: string): Promise<Tag> { ... }
export async function deleteTag(id: number): Promise<void> { ... }
export async function tagFile(fileId: string, tagId: number): Promise<void> { ... }
export async function untagFile(fileId: string, tagId: number): Promise<void> { ... }
export async function getFileTags(fileId: string): Promise<Tag[]> { ... }
```

**文件：`src/components/PreviewPanel.tsx`**

在元数据区域新增标签展示和编辑：

```tsx
// 标签区域
<div className="flex flex-wrap gap-1 mb-2">
  {tags.map(tag => (
    <span key={tag.id} className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs"
      style={{ backgroundColor: tag.color + '20', color: tag.color }}>
      {tag.name}
      <button onClick={() => untagFile(fileId, tag.id)}>×</button>
    </span>
  ))}
  <button onClick={() => setShowTagPicker(true)} className="text-xs text-blue-500">+</button>
</div>
```

**文件：`src/components/FilterPanel.tsx`**

在筛选面板新增「标签」区域，显示标签云，点击即可过滤。

### 搜索语法

- `tag:合同` → 搜索标签为"合同"的文件
- `tag:合同 AND tag:供应商` → 同时有这两个标签的文件
- 标签自动补全：输入 `tag:` 时弹出已知标签列表

### 验证

1. 在预览面板给文件添加标签
2. 搜索 `tag:合同` → 返回正确文件
3. 标签云显示在筛选面板，点击过滤
4. 删除标签 → 所有关联自动解除
5. 重启后标签持久化

---

## 2.4 前进/后退导航栈

### 目标

浏览器式 back/forward，记住"搜索 → 浏览 → 预览 → 再搜索"的完整路径。

### 设计

**文件：`src/hooks/useNavigationStack.ts`**（新建）

```tsx
interface NavEntry {
  type: 'search' | 'file' | 'browse' | 'collection'
  label: string
  timestamp: number
  // 不同 type 的 payload
  payload: SearchPayload | FilePayload | BrowsePayload | CollectionPayload
}

function useNavigationStack() {
  const [stack, setStack] = useState<NavEntry[]>([])
  const [cursor, setCursor] = useState(-1)

  const push = (entry: NavEntry) => {
    // 截断 cursor 之后的历史
    setStack(prev => [...prev.slice(0, cursor + 1), entry])
    setCursor(prev => prev + 1)
  }

  const back = () => {
    if (cursor > 0) {
      setCursor(prev => prev - 1)
      restore(stack[cursor - 1])
    }
  }

  const forward = () => {
    if (cursor < stack.length - 1) {
      setCursor(prev => prev + 1)
      restore(stack[cursor + 1])
    }
  }

  return { stack, cursor, push, back, forward, canGoBack, canGoForward }
}
```

### 集成

**文件：`src/pages/SearchPage.tsx`**

```tsx
// 搜索执行时
const nav = useNavigationStack()
const submitSearch = () => {
  nav.push({ type: 'search', label: query, payload: { query, filters, page } })
  executeSearch(...)
}

// 打开文件预览时
const handleSelectHit = (hit: SearchHit) => {
  nav.push({ type: 'file', label: hit.file_name, payload: { fileId: hit.file_id } })
  setSelectedHit(hit)
}
```

**UI：** 在搜索框左侧或状态栏显示前进/后退按钮：

```
[←] [→] [搜索框 ...]
```

或绑定快捷键 `Alt+←` / `Alt+→`（参考 Everything）。

### 验证

1. 搜索 → 打开文件 → 按 `Alt+←` → 回到搜索结果
2. 按 `Alt+→` → 回到文件预览
3. 在新搜索后，旧的前进历史被截断
4. 导航栈在搜索框左侧有视觉指示（可点击的 ← → 按钮）

---

# 第三波：中文增强 + 打磨

> ~2 周

---

## 3.1 拼音搜索

### 目标

输入 `pinyin:nihao` 匹配包含 "你好" 的文件。

### 实现方案

**方案一：Tantivy 拼音分词器（推荐）**

在 Tantivy schema 中新增 `content_pinyin` 字段，使用 jieba 分词 + 拼音转换：

```rust
// 现有 schema 扩展
schema_builder.add_text_field("content_pinyin", TEXT | STORED);

// 索引时，对中文文本生成拼音 token
// 例如 "你好" → "ni hao"、"nihao"
```

依赖：`pinyin` crate（或 `zh-pinyin`），将中文文本转拼音。

**方案二：搜索时拼音转换（简化版）**

```rust
// 搜索时检测 pinyin: 前缀
// 将拼音字符串通过拼音→中文映射表或 jieba 拼音词典转换
// 将转换后的中文词作为 OR 条件加入搜索
```

**推荐方案一**，与现有 jieba 分词器模式一致，索引时转换，搜索时无额外开销。

### 文件

**文件：`src-tauri/src/search/schema.rs`**

```rust
// 新增字段
schema_builder.add_text_field("content_pinyin", TEXT | STORED);

// 注册拼音分词器
// 或复用 jieba 分词器 + 拼音后处理
```

**文件：`src-tauri/src/indexer.rs`**

```rust
// Phase 1 提取文本后，生成拼音 token
let pinyin_text = convert_to_pinyin(&extracted_text);
// 写入 content_pinyin 字段
```

**文件：`src-tauri/src/search/searcher.rs`**

```rust
// 搜索时处理 pinyin: 前缀
// 拼音搜索走 content_pinyin 字段
```

### 验证

1. 索引包含 "你好" 的文件
2. 搜索 `pinyin:nihao` → 匹配
3. 搜索 `pinyin:ni hao` → 匹配（支持分词）
4. 拼音搜索与关键词搜索同时使用（`pinyin:hetong AND 2025`）

---

## 3.2 预览折叠（HoudahSpot 模式）

### 目标

预览面板默认只显示搜索匹配段落，而非全文，方便快速浏览多条结果。

### 修改

**文件：`src/components/PreviewPanel.tsx`**

新增折叠/展开切换：

```tsx
const [foldMode, setFoldMode] = useState(true)  // 默认折叠

// 折叠模式下，只显示匹配行及其上下文（±2 行）
const foldedContent = useMemo(() => {
  if (!foldMode || !searchQuery.trim()) return textContent
  const lines = textContent.split('\n')
  const terms = searchQuery.toLowerCase().split(/\s+/)
  const matchLines = new Set<number>()
  lines.forEach((line, i) => {
    const lower = line.toLowerCase()
    if (terms.some(t => lower.includes(t))) {
      // 匹配行 ±2 行
      for (let j = Math.max(0, i - 2); j <= Math.min(lines.length - 1, i + 2); j++) {
        matchLines.add(j)
      }
    }
  })
  return Array.from(matchLines).sort((a, b) => a - b).map(i => lines[i]).join('\n')
}, [textContent, searchQuery, foldMode])
```

**UI：**

```tsx
// 匹配导航栏旁新增折叠切换
<button
  onClick={() => setFoldMode(v => !v)}
  className="text-xs px-2 py-1 rounded hover:bg-gray-100"
>
  {foldMode ? '显示全文' : '仅匹配段落'}
</button>

// 折叠模式下显示匹配行数
<span className="text-xs text-gray-400">
  显示 {matchLines.length} / {totalLines} 行
</span>
```

### 验证

1. 搜索 "合同" → 预览面板默认只显示包含 "合同" 的段落 ±2 行
2. 切换「显示全文」→ 回退到现有全文显示
3. 匹配计数 + 上下翻页在折叠模式下正常工作
4. 在折叠模式下，匹配高亮依然生效

---

## 3.3 键盘快捷键手册

### 目标

按 `?` 或 `⌘/` 显示快捷键参考面板，帮助用户发现所有键盘操作。

### 实现

**文件：`src/components/KeyboardShortcuts.tsx`（新建）**

```tsx
export default function KeyboardShortcuts() {
  const shortcuts = [
    { keys: ['⌘K'], description: '聚焦搜索框' },
    { keys: ['↑', '↓'], description: '导航搜索结果' },
    { keys: ['Enter'], description: '打开所选文件' },
    { keys: ['Esc'], description: '清空搜索 / 关闭预览' },
    { keys: ['Alt', '↑'], description: '上一条搜索历史' },
    { keys: ['Alt', '↓'], description: '下一条搜索历史' },
    { keys: ['Alt', '←'], description: '后退（导航历史）' },
    { keys: ['Alt', '→'], description: '前进（导航历史）' },
    { keys: ['Space'], description: '快速预览（Quick Look）' },
    { keys: ['Tab'], description: '切换焦点（搜索框 ↔ 结果 ↔ 预览）' },
    { keys: ['Ctrl', 'C'], description: '复制文件路径' },
    { keys: ['Ctrl', 'Enter'], description: '在文件管理器中定位' },
    { keys: ['Ctrl', 'D'], description: '保存当前搜索' },
    { keys: ['Ctrl', 'E'], description: '导出搜索结果' },
    { keys: ['?'], description: '显示快捷键帮助' },
  ]
  // 模态面板，半透明背景，居中显示快捷键列表
}
```

**文件：`src/pages/SearchPage.tsx`**

```tsx
// 监听 ? 键
useEffect(() => {
  const handler = (e: KeyboardEvent) => {
    if (e.key === '?' && !isInputFocused()) {
      setShowShortcuts(true)
    }
  }
  document.addEventListener('keydown', handler)
  return () => document.removeEventListener('keydown', handler)
}, [])
```

### 验证

1. 按 `?` → 显示快捷键面板
2. 面板显示所有支持的快捷键
3. 按 `Esc` 关闭面板

---

## 3.4 全局快捷键：聚焦搜索框

### 现状

搜索框 placeholder 显示 `(⌘K)` 但并无实际绑定。

### 修改

**文件：`src/App.tsx`**

```tsx
useEffect(() => {
  const handler = (e: KeyboardEvent) => {
    // 不在输入框中时，⌘K 聚焦搜索框
    if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
      e.preventDefault()
      const searchInput = document.querySelector('[data-search-input]') as HTMLInputElement
      searchInput?.focus()
    }
  }
  document.addEventListener('keydown', handler)
  return () => document.removeEventListener('keydown', handler)
}, [])
```

### 验证

1. 在任何页面按 `⌘K` → 搜索框获得焦点
2. 搜索框已有输入时按 `⌘K` → 选中全部文本
3. 在 AI 聊天页按 `⌘K` → 无影响（聊天页有自己的快捷键）

---

## 3.5 搜索结果批量操作

### 目标

在 ResultList 中支持多选，对选中的文件执行批量操作。

### 修改

**文件：`src/components/ResultList.tsx`**

```tsx
// 新增状态
const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set())

// 点击行：单选（取消已有选中）
// Cmd/Ctrl+点击：切换选中
// Shift+点击：区间选择
const handleClick = (e: React.MouseEvent, hit: SearchHit) => {
  if (e.metaKey || e.ctrlKey) {
    // 切换
    setSelectedIds(prev => {
      const next = new Set(prev)
      if (next.has(hit.file_id)) next.delete(hit.file_id)
      else next.add(hit.file_id)
      return next
    })
  } else if (e.shiftKey && lastSelected.current) {
    // 区间选择
    const range = getRange(hits, lastSelected.current, hit.file_id)
    setSelectedIds(prev => {
      const next = new Set(prev)
      range.forEach(id => next.add(id))
      return next
    })
  } else {
    setSelectedIds(new Set([hit.file_id]))
    onSelect(hit)
  }
}
```

**批量操作栏**（选中 2+ 项时显示在结果列表底部）：

```tsx
<div className="flex items-center gap-2 px-4 py-2 bg-blue-50 border-t border-blue-100">
  <span className="text-xs text-blue-700">已选择 {selectedIds.size} 个文件</span>
  <button onClick={batchOpen}>全部打开</button>
  <button onClick={batchExport}>导出</button>
  <button onClick={batchCopyPath}>复制路径</button>
  <button onClick={batchTag}>添加标签</button>
  <button onClick={batchAddToCollection}>加入收藏</button>
  <button onClick={() => setSelectedIds(new Set())}>取消选择</button>
</div>
```

### 验证

1. 点击单个结果 → 单选，打开预览
2. `Cmd+点击` → 切换选中/取消
3. `Shift+点击` → 区间选择
4. 选中 3 个文件 → 批量操作栏出现
5. 批量添加标签 → 所有选中文件获得该标签
6. 批量导出 → 导出选中文件列表

---

# 附录：数据模型变更汇总

## 表结构变更

| 表 | 操作 | 说明 |
|-----|--------|------|
| `search_history` | 扩展列 | 加 `clicked_results`、`sort_field`、`filters` |
| `search_history_clicks` | 新增 | 点击追踪 |
| `saved_searches` | 新增 | 保存搜索 = 智能文件夹 |
| `tags` | 新增 | 标签定义 |
| `file_tags` | 新增 | 文件-标签关联 |
| `doc_embeddings` | 加索引 | 按 `kb_id` 查询优化（可选） |

## 前端文件变更清单

| 文件 | 操作 | 波次 |
|------|--------|------|
| `src/components/SearchBar.tsx` | 改：加建议下拉 + 历史下拉 | 1 |
| `src/components/ResultList.tsx` | 改：加多选 + 批量操作 | 2 |
| `src/components/PreviewPanel.tsx` | 改：加标签编辑 + 折叠模式 | 2, 3 |
| `src/components/FilterPanel.tsx` | 改：加标签云区域 | 2 |
| `src/components/KeyboardShortcuts.tsx` | 新建 | 3 |
| `src/hooks/useSearch.ts` | 改：加 fuzzy/date 状态 | 1 |
| `src/hooks/useNavigationStack.ts` | 新建 | 2 |
| `src/pages/SearchPage.tsx` | 改：传递新 props + 保存搜索按钮 | 1, 2 |
| `src/App.tsx` | 改：侧边栏加「已保存搜索」+ ⌘K 全局快捷键 | 2, 3 |
| `src/api/search.ts` | 改：加保存搜索 API + 历史点击 API | 1, 2 |
| `src/api/files.ts` | 改：加标签 API | 2 |

## 后端文件变更清单

| 文件 | 操作 | 波次 |
|------|--------|------|
| `src-tauri/src/commands/search.rs` | 改：加保存搜索/历史记录命令 | 1, 2 |
| `src-tauri/src/commands/tags.rs` | 新建：标签 CRUD | 2 |
| `src-tauri/src/commands/files.rs` | 改：加标签文件/取消标签 | 2 |
| `src-tauri/src/commands/saved_searches.rs` | 新建（或放 search.rs 中） | 2 |
| `src-tauri/src/db/mod.rs` | 改：加新表创建 | 2 |
| `src-tauri/src/db/search_history.rs` | 改：扩展历史记录表 | 2 |
| `src-tauri/src/search/schema.rs` | 改：加拼音搜索字段 | 3 |
| `src-tauri/src/search/searcher.rs` | 改：加拼音搜索 + tag: 解析 | 2, 3 |
| `src-tauri/src/indexer.rs` | 改：加拼音生成 | 3 |

---

## 实施建议

1. **第一波按顺序做**：建议 → 历史 → 模糊/日期，每项独立可测试
2. **第二波并行可能**：标签（独立于搜索）可与其他项并行开发
3. **第三波依赖第二波**：拼音搜索依赖 schema 变更，建议在索引重建时一起做
4. **每次提交前**：`cargo check` + `lsp_diagnostics` + 更新 `CHANGELOG.md`