// Web 端会话相关的前端行为测试（vitest 默认 node 环境，手动装最小浏览器桩）。
//
// 覆盖 `client.ts` 本次新增/改动的关键分支：
// - `web_session_*` 三个命令的 HTTP 映射是否正确
// - 403（会话被占用）→ 派发 `session-denied` 且**不清除本地 token**
// - 401（token 失效）→ 清除本地 token 并派发 `auth-failed`
//
// 说明：会话遮罩 UI（App.tsx）与 30s 心跳是 React 组件行为，需要 DOM 测试环境，
// 这里只覆盖可纯函数化、且最容易回归的请求层逻辑。

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { invoke } from '../client'

interface Captured {
  type: string
  detail?: unknown
}

let events: Captured[]
let store: Record<string, string>
let fetchSpy: ReturnType<typeof vi.fn>

function installBrowserStubs() {
  events = []
  store = { ls_token: 'tok-123' }

  ;(globalThis as unknown as { localStorage: unknown }).localStorage = {
    getItem: (k: string) => (k in store ? store[k] : null),
    setItem: (k: string, v: string) => {
      store[k] = v
    },
    removeItem: (k: string) => {
      delete store[k]
    },
  }

  ;(globalThis as unknown as { window: unknown }).window = {
    location: { origin: 'https://ls.test' },
    dispatchEvent: (e: Event & { detail?: unknown }) => {
      events.push({ type: e.type, detail: e.detail })
      return true
    },
    addEventListener: () => {},
    removeEventListener: () => {},
  }

  fetchSpy = vi.fn()
  ;(globalThis as unknown as { fetch: unknown }).fetch = fetchSpy
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

beforeEach(() => {
  installBrowserStubs()
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('web session HTTP 映射', () => {
  it('web_session_status 映射到 GET /api/session', async () => {
    fetchSpy.mockResolvedValue(jsonResponse(200, { active: false }))
    await invoke('web_session_status')
    expect(fetchSpy).toHaveBeenCalledTimes(1)
    const [url, init] = fetchSpy.mock.calls[0]
    expect(url).toBe('https://ls.test/api/session')
    expect((init as RequestInit).method).toBe('GET')
  })

  it('web_session_ping 映射到 POST /api/session/ping', async () => {
    fetchSpy.mockResolvedValue(jsonResponse(200, { active: true }))
    await invoke('web_session_ping')
    const [url, init] = fetchSpy.mock.calls[0]
    expect(url).toBe('https://ls.test/api/session/ping')
    expect((init as RequestInit).method).toBe('POST')
  })

  it('web_session_logout 映射到 POST /api/session/logout', async () => {
    fetchSpy.mockResolvedValue(jsonResponse(200, { status: 'logged_out' }))
    await invoke('web_session_logout')
    const [url, init] = fetchSpy.mock.calls[0]
    expect(url).toBe('https://ls.test/api/session/logout')
    expect((init as RequestInit).method).toBe('POST')
  })

  it('请求带上当前 Bearer token', async () => {
    fetchSpy.mockResolvedValue(jsonResponse(200, {}))
    await invoke('web_session_status')
    const [, init] = fetchSpy.mock.calls[0]
    const headers = (init as RequestInit).headers as Record<string, string>
    expect(headers.Authorization).toBe('Bearer tok-123')
  })
})

describe('403 会话被占用', () => {
  it('派发 session-denied（带 IP 与剩余秒数）且不清除本地 token', async () => {
    fetchSpy.mockResolvedValue(
      jsonResponse(403, {
        error: '已由 192.168.1.9 登录（单用户模式）…',
        session_ip: '192.168.1.9',
        expires_in: 500,
      }),
    )

    await expect(invoke('web_session_status')).rejects.toThrow()

    const denied = events.find(e => e.type === 'session-denied')
    expect(denied).toBeDefined()
    expect(denied!.detail).toMatchObject({ session_ip: '192.168.1.9', expires_in: 500 })

    // 关键：被占不清 token——对方退出/超时后原 token 仍可用
    expect(store.ls_token).toBe('tok-123')
    expect(events.some(e => e.type === 'auth-failed')).toBe(false)
  })

  it('403 响应体不是 JSON 时也能派发事件（不炸）', async () => {
    fetchSpy.mockResolvedValue(new Response('nope', { status: 403 }))
    await expect(invoke('web_session_ping')).rejects.toThrow()
    expect(events.some(e => e.type === 'session-denied')).toBe(true)
  })
})

describe('401 token 失效', () => {
  it('清除本地 token 并派发 auth-failed', async () => {
    fetchSpy.mockResolvedValue(jsonResponse(401, { error: 'unauthorized' }))

    await expect(invoke('web_session_status')).rejects.toThrow()

    expect(store.ls_token).toBeUndefined()
    expect(events.some(e => e.type === 'auth-failed')).toBe(true)
    expect(events.some(e => e.type === 'session-denied')).toBe(false)
  })
})
