// 轻量全局 toast：模块级事件总线 + 单例容器，无 Context。
// 任意组件 import { toast } from '../utils/toast' 即可调用：
//   toast('已复制到剪贴板')            // 默认 success
//   toast('导出失败', 'error')         // error 红色
//   toast('处理中', 'info')            // info 灰色

export type ToastType = 'success' | 'error' | 'info'

export interface ToastItem {
  id: number
  message: string
  type: ToastType
}

type Listener = (items: ToastItem[]) => void

let items: ToastItem[] = []
let listeners = new Set<Listener>()
let nextId = 1
const DURATION_MS = 2500

function emit() {
  for (const l of listeners) l(items)
}

function remove(id: number) {
  items = items.filter(t => t.id !== id)
  emit()
}

export function toast(message: string, type: ToastType = 'success') {
  const id = nextId++
  items = [...items, { id, message, type }]
  emit()
  setTimeout(() => remove(id), DURATION_MS)
}

export function dismissToast(id: number) {
  remove(id)
}

export function subscribeToasts(l: Listener): () => void {
  listeners.add(l)
  l(items) // 立即回放当前队列（容器挂载/热更时同步）
  return () => { listeners.delete(l) }
}
