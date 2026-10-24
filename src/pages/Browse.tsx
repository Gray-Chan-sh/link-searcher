import { useCallback, useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { toast } from '../utils/toast'
import type { DirEntry } from '../api/files'
import { useDirs } from '../hooks/useDirs'
import PreviewPanel from '../components/PreviewPanel'
import FolderView from '../components/browse/FolderView'

/**
 * 浏览页 = **文件夹视图**（Windows 资源管理器风格）。
 *
 * 职责边界（2026-10-24 与用户确认）：
 * - 浏览页只做"**按目录看文件**"：进入/返回目录、当前目录内按名字/路径过滤；
 * - 全库关键字检索归**搜索页**（这里不再内嵌全文搜索，也不再提供跨目录平铺列表）；
 * - 右侧预览面板与搜索页共用（字号/宽度/Markdown 渲染一致）。
 */
export default function Browse() {
  const [params] = useSearchParams()
  /** 引用深链 `?path=<库内相对路径>`：文件夹视图会打开其父目录并选中。 */
  const deepLinkPath = useMemo(() => params.get('path'), [params])
  const { dirs } = useDirs()
  const [selectedFile, setSelectedFile] = useState<{ fileId?: string; path: string } | null>(null)

  /** 单击某一行 → 更新右侧预览：已索引用 id（库内正文），其余按绝对路径直读磁盘。 */
  const handleFolderSelect = useCallback((entry: DirEntry) => {
    setSelectedFile({
      fileId: entry.indexed === 1 ? (entry.file_id ?? undefined) : undefined,
      path: entry.path,
    })
  }, [])

  return (
    <div className="flex h-full">
      <div className="flex-1 flex flex-col min-w-0 border-r border-gray-200 dark:border-gray-800">
        <FolderView
          roots={dirs}
          deepLinkPath={deepLinkPath}
          selectedPath={selectedFile?.path ?? null}
          onSelectEntry={handleFolderSelect}
          onNotice={msg => toast(msg)}
        />
      </div>

      <PreviewPanel
        fileId={selectedFile?.fileId ?? null}
        path={selectedFile?.fileId ? null : (selectedFile?.path ?? null)}
        searchQuery=""
        onClose={() => setSelectedFile(null)}
        collapsible
      />
    </div>
  )
}
