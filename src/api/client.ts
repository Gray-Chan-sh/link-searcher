// Unified API client: routes invoke calls to Tauri IPC or HTTP fetch automatically.
// All src/api/*.ts files should use `client.invoke()` instead of `invoke()` from @tauri-apps/api/core.

import { isTauri, getApiBase, getToken } from '../utils/platform';

export type InvokeArgs = Record<string, unknown>;

/** Check if a command maps to an HTTP endpoint (for browser mode). */
function getHttpPath(command: string, args: InvokeArgs): { method: string; path: string; body?: unknown } | null {
  // Map Tauri command names to REST API endpoints
  const mappings: Record<string, { method: string; path: string; param?: string } | ((args: InvokeArgs) => { method: string; path: string; body?: unknown })> = {
    search: { method: 'GET', path: '/api/search' },
    suggest: { method: 'GET', path: '/api/suggest' },
    get_file: { method: 'GET', path: '/api/files' },
    list_files_db: { method: 'GET', path: '/api/files' },
    get_duplicates: { method: 'GET', path: '/api/files' },
    preview_file: (a) => ({ method: 'GET', path: `/api/files/${a.id}/preview` }),
    get_file_preview: (a) => ({ method: 'GET', path: `/api/files/${a.id}/preview` }),
    preview_file_by_path: (a) => ({ method: 'GET', path: `/api/files/${a.path}/preview` }),
    get_index_status: { method: 'GET', path: '/api/index/status' },
    check_index_health: { method: 'GET', path: '/api/index/health' },
    list_dirs: { method: 'GET', path: '/api/dirs' },
    ai_capabilities: { method: 'GET', path: '/api/ai/capabilities' },
    get_version: { method: 'GET', path: '/api/version' },
    get_settings: { method: 'GET', path: '/api/settings' },
    list_chat_sessions: { method: 'GET', path: '/api/chat/sessions' },
    trigger_scan: { method: 'POST', path: '/api/scan/trigger' },
    cancel_scan: { method: 'POST', path: '/api/scan/cancel' },
    reindex_file: (a) => ({ method: 'POST', path: '/api/reindex', body: a }),
    create_chat_session: { method: 'POST', path: '/api/chat/sessions' },
    load_chat_session: (a) => ({ method: 'GET', path: `/api/chat/sessions/${a.id}` }),
    delete_chat_session: (a) => ({ method: 'DELETE', path: `/api/chat/sessions/${a.id}` }),
    export_chat_session: (a) => ({ method: 'POST', path: `/api/chat/sessions/${a.id}/export` }),
    export_chat_session_json: (a) => ({ method: 'POST', path: `/api/chat/sessions/${a.id}/export` }),
    ask_documents: { method: 'POST', path: '/api/chat/ask' },
  };

  const entry = mappings[command];
  if (!entry) return null;

  const spec = typeof entry === 'function' ? entry(args) : entry;
  const queryParts: string[] = [];
  for (const [k, v] of Object.entries(args)) {
    if (v === undefined || v === null) continue;
    if (Array.isArray(v)) {
      if (v.length > 0) queryParts.push(`${k}=${v.join(',')}`);
    } else {
      queryParts.push(`${k}=${encodeURIComponent(String(v))}`);
    }
  }
  const qs = queryParts.length ? '?' + queryParts.join('&') : '';
  return {
    method: spec.method,
    path: spec.path + qs,
    body: spec.method !== 'GET' && spec.method !== 'DELETE' ? args : undefined,
  };
}

/** Unified invoke: Tauri IPC or HTTP fetch. */
export async function invoke<T = unknown>(command: string, args?: InvokeArgs): Promise<T> {
  if (isTauri()) {
    const { invoke: tauriInvoke } = await import('@tauri-apps/api/core');
    return tauriInvoke<T>(command, args || {});
  }

  // Browser mode: route to HTTP
  const httpSpec = getHttpPath(command, args || {});
  if (!httpSpec) {
    // Command not yet mapped — return empty results rather than null
    // ponytail: map more commands to HTTP endpoints as needed
    console.warn(`[client] No HTTP mapping for "${command}", returning empty fallback`);
    return ([] as unknown) as T;
  }

  const url = getApiBase() + httpSpec.path;
  const headers: Record<string, string> = {
    'Authorization': `Bearer ${getToken()}`,
  };
  if (httpSpec.body) headers['Content-Type'] = 'application/json';

  const resp = await fetch(url, {
    method: httpSpec.method,
    headers,
    body: httpSpec.body ? JSON.stringify(httpSpec.body) : undefined,
  });

  if (resp.status === 401) {
    throw new Error('Token 无效或缺失');
  }
  if (!resp.ok) {
    const err = await resp.json().catch(() => ({ error: `HTTP ${resp.status}` }));
    throw new Error(err.error || `HTTP ${resp.status}`);
  }

  return resp.json();
}

/** Unified event listener: Tauri events or SSE. Returns an unlisten function. */
export async function listen<T = unknown>(
  event: string,
  handler: (payload: T) => void,
  _sessionId?: string,
): Promise<() => void> {
  if (isTauri()) {
    const { listen: tauriListen } = await import('@tauri-apps/api/event');
    return tauriListen<T>(event, (e) => handler(e.payload));
  }

  // Browser mode: SSE for streaming events
  // For now, polling fallback for non-streaming events
  // TODO: Add SSE endpoint for each event type
  const sseEndpoints: Record<string, string> = {
    'ai-chunk': `/api/ai/sse`,
    'ai-done': `/api/ai/sse`,
    'scan-progress': `/api/index/sse`,
    'scan-completed': `/api/index/sse`,
  };

  const ssePath = sseEndpoints[event];
  if (!ssePath) {
    // No SSE endpoint for this event — return no-op
    return () => {};
  }

  const es = new EventSource(`${getApiBase()}${ssePath}`, {
    withCredentials: false,
  });

  es.addEventListener(event, (e: MessageEvent) => {
    try {
      handler(JSON.parse(e.data));
    } catch {
      handler(e.data as unknown as T);
    }
  });

  es.addEventListener('error', () => {
    console.warn(`[client] SSE error for event "${event}"`);
  });

  return () => es.close();
}
