import { useEffect, useState } from 'react'

/**
 * 视口是否窄于断点（默认 1024px，即 Tailwind 的 `lg`）。
 *
 * 原先 `Browse` / `SearchPage` / `FilterPanel` / `PreviewPanel` 各自复制了一份
 * 一模一样的实现，改动断点时容易漏掉其中一处 —— 统一收到这里。
 */
export function useIsMobile(breakpoint = 1024) {
  const [isMobile, setIsMobile] = useState(() => window.innerWidth < breakpoint)
  useEffect(() => {
    const mq = window.matchMedia(`(max-width: ${breakpoint - 1}px)`)
    const handler = (e: MediaQueryListEvent) => setIsMobile(e.matches)
    mq.addEventListener('change', handler)
    setIsMobile(mq.matches)
    return () => mq.removeEventListener('change', handler)
  }, [breakpoint])
  return isMobile
}
