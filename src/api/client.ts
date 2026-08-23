// Unified API client: routes invoke calls to Tauri IPC or HTTP fetch automatically.
import { isTauri, getApiBase, getToken } from '../utils/platform';

export type InvokeArgs = Record<string, unknown>;

interface HttpSpec {
  method: string;
  path: string;
  body?: unknown;
  transform?: (data: unknown, args: InvokeArgs) => unknown;
}

function buildQuery(args: InvokeArgs): string {
  const parts: string[] = [];
  for (const [k, v] of Object.entries(args)) {
    if (v === undefined || v === null) continue;
    if (k === 'scope' || k === 'session' || typeof v === 'object') continue;
    if (Array.isArray(v)) {
      if (v.length > 0) parts.push(`${k}=${v.join(',')}`);
    } else {
      parts.push(`${k}=${encodeURIComponent(String(v))}`);
    }
  }
  return parts.length ? '?' + parts.join('&') : '';
}

// Command → HTTP endpoint mapping with response transformation
const MAPPINGS: Record<string, HttpSpec | ((args: InvokeArgs) => HttpSpec)> = {
  // Search
  search: { method: 'GET', path: '/api/search' },
  suggest: { method: 'GET', path: '/api/suggest' },

  // Files
  list_files_db: { method: 'GET', path: '/api/files', transform: (data) => {
    const d = data as Record<string, unknown>;
    return { items: d.files ?? [], total: d.total ?? 0, page: d.page ?? 1, page_size: d.page_size ?? 50 };
  }},
  get_file: (a) => ({ method: 'GET', path: `/api/files/${a.id}` }),
  get_duplicates: { method: 'GET', path: '/api/files', transform: () => [] },
  preview_file: (a) => ({ method: 'GET', path: `/api/files/${a.id}/preview` }),
  get_file_preview: (a) => ({ method: 'GET', path: `/api/files/${a.id}/preview` }),
  preview_file_by_path: (a) => ({ method: 'GET', path: `/api/files/${a.path}/preview` }),

  // Index
  get_index_status: { method: 'GET', path: '/api/index/status', transform: (data) => {
    const d = data as Record<string, unknown>;
    return {
      total_files: d.total ?? 0,
      indexed: d.indexed ?? 0,
      pending: d.pending ?? 0,
      errors: d.failed ?? 0,
      ocred: 0,
      total_images: 0,
      last_scan: null,
      is_scanning: !!d.is_scanning,
      scan_delta: undefined,
      running_tasks: [],
      briefs: [],
    };
  }},
  check_index_health: { method: 'GET', path: '/api/index/health' },
  trigger_scan: { method: 'POST', path: '/api/scan/trigger' },
  cancel_scan: { method: 'POST', path: '/api/scan/cancel' },
  reindex_file: (a) => ({ method: 'POST', path: '/api/reindex', body: a }),

  // Dirs
  list_dirs: {
    method: 'GET', path: '/api/dirs',
    transform: (data) => (data as Record<string, unknown>)?.dirs ?? data ?? [],
  },
  get_dir_tree: { method: 'GET', path: '/api/dirs', transform: (data) => {
    const d = data as Record<string, unknown>;
    const dirs = (d?.dirs ?? []) as Record<string, unknown>[];
    return {
      name: 'root',
      path: '/',
      is_dir: true,
      children: dirs.map(dir => ({
        name: String((dir.path as string)?.split('/')?.pop() || dir.alias || ''),
        path: String(dir.path || ''),
        is_dir: true,
        children: [] as Record<string, unknown>[],
      })),
    };
  }},
  get_dir_children: { method: 'GET', path: '/api/dirs' },
  add_dir: { method: 'POST', path: '/api/dirs' },
  remove_dir: { method: 'DELETE', path: '/api/dirs' },
  update_dir: { method: 'PUT', path: '/api/dirs' },

  // AI
  ai_capabilities: { method: 'GET', path: '/api/ai/capabilities' },
  ask_documents: { method: 'POST', path: '/api/chat/ask' },
  summarize_file: (a) => ({ method: 'POST', path: '/api/chat/ask', body: { question: `总结文件 ${a.fileId}` } }),

  // Chat
  list_chat_sessions: {
    method: 'GET', path: '/api/chat/sessions',
    transform: (data) => (data as Record<string, unknown>)?.sessions ?? data ?? [],
  },
  create_chat_session: { method: 'POST', path: '/api/chat/sessions' },
  load_chat_session: (a) => ({ method: 'GET', path: `/api/chat/sessions/${a.id}` }),
  delete_chat_session: (a) => ({ method: 'DELETE', path: `/api/chat/sessions/${a.id}` }),
  export_chat_session: (a) => ({ method: 'POST', path: `/api/chat/sessions/${a.id}/export` }),
  export_chat_session_json: (a) => ({ method: 'POST', path: `/api/chat/sessions/${a.id}/export` }),

  // Settings
  get_settings: { method: 'GET', path: '/api/settings' },
  update_settings: { method: 'PUT', path: '/api/settings' },
  get_version: { method: 'GET', path: '/api/version' },

  // Search history
  get_search_history: { method: 'GET', path: '/api/settings', transform: () => [] },
  clear_search_history: { method: 'POST', path: '/api/settings', transform: () => null },
  get_file_type_stats: { method: 'GET', path: '/api/files', transform: () => [] },
  get_browse_file_types: { method: 'GET', path: '/api/files', transform: (data) => {
    const d = data as Record<string, unknown>;
    if (d?.files && Array.isArray(d.files)) return [...new Set((d.files as Record<string,unknown>[]).map((f) => (f as Record<string,string>).file_ext).filter(Boolean))];
    return [];
  }},

  // Backup
  get_backup_status: { method: 'GET', path: '/api/settings', transform: () => ({ last_backup: 0, size: 0, count: 0 }) },
  list_backups: { method: 'GET', path: '/api/settings', transform: () => [] },
  trigger_backup: { method: 'POST', path: '/api/scan/trigger', transform: () => null },
  delete_backup: { method: 'POST', path: '/api/scan/cancel', transform: () => null },
  get_dead_dirs: { method: 'GET', path: '/api/dirs', transform: () => [] },
  remap_dir: { method: 'POST', path: '/api/dirs', transform: () => null },
  remove_dir_with_files: { method: 'DELETE', path: '/api/dirs', transform: () => null },

  // Config
  get_config: { method: 'GET', path: '/api/settings' },
  test_ai_gateway: { method: 'GET', path: '/api/ai/capabilities' },
  cancel_ai_request: { method: 'POST', path: '/api/scan/cancel', transform: () => null },

  // OCR
  check_tesseract: { method: 'GET', path: '/api/version', transform: () => true },
  list_ocr_engines: { method: 'GET', path: '/api/settings', transform: () => [] },
  test_ocr_engine: { method: 'GET', path: '/api/version', transform: () => ({ ok: false, message: 'Not available in Web mode' }) },
  check_dependencies: { method: 'GET', path: '/api/settings', transform: () => [] },
  get_file_type_support: { method: 'GET', path: '/api/settings', transform: () => [] },
  get_unsupported_ext_stats: { method: 'GET', path: '/api/settings', transform: () => [] },
  check_bge_installed: { method: 'GET', path: '/api/version', transform: () => false },

  // Desktop-only (return safe defaults in Web mode)
  open_file: { method: 'GET', path: '/api/version', transform: () => null },
  reveal_in_folder: { method: 'GET', path: '/api/version', transform: () => null },
  rebuild_index: { method: 'POST', path: '/api/scan/trigger', transform: () => null },
  restart_app: { method: 'POST', path: '/api/scan/cancel', transform: () => null },
  update_config: { method: 'PUT', path: '/api/settings', transform: () => null },
  update_provider: { method: 'PUT', path: '/api/settings', transform: () => null },
  delete_provider: { method: 'DELETE', path: '/api/settings', transform: () => null },
  set_active_model: { method: 'PUT', path: '/api/settings', transform: () => null },
  install_bge: { method: 'POST', path: '/api/scan/trigger', transform: () => null },
  install_funasr: { method: 'POST', path: '/api/scan/trigger', transform: () => null },
  clear_logs: { method: 'POST', path: '/api/scan/cancel', transform: () => null },
};

export async function invoke<T = unknown>(command: string, args?: InvokeArgs): Promise<T> {
  if (isTauri()) {
    const { invoke: tauriInvoke } = await import('@tauri-apps/api/core');
    return tauriInvoke<T>(command, args || {});
  }

  const entry = MAPPINGS[command];
  if (!entry) {
    console.warn(`[client] No mapping for "${command}"`);
    return ([] as unknown) as T;
  }

  const spec = typeof entry === 'function' ? entry(args || {}) : entry;
  const qs = spec.method === 'GET' || spec.method === 'DELETE' ? buildQuery(args || {}) : '';
  const url = getApiBase() + spec.path + qs;

  const headers: Record<string, string> = { Authorization: `Bearer ${getToken()}` };
  if (spec.body) headers['Content-Type'] = 'application/json';

  const resp = await fetch(url, {
    method: spec.method,
    headers,
    body: spec.body ? JSON.stringify(spec.body) : undefined,
  });

  if (resp.status === 401) throw new Error('Token 无效');
  if (!resp.ok) {
    const err = await resp.json().catch(() => ({ error: `HTTP ${resp.status}` }));
    throw new Error(err.error || `HTTP ${resp.status}`);
  }

  const data = await resp.json();
  return (spec.transform ? spec.transform(data, args || {}) : data) as T;
}

export async function listen<T = unknown>(
  event: string,
  handler: (payload: T) => void,
  _sessionId?: string,
): Promise<() => void> {
  if (isTauri()) {
    const { listen: tauriListen } = await import('@tauri-apps/api/event');
    return tauriListen<T>(event, (e) => handler(e.payload));
  }
  // ponytail: SSE endpoints not yet implemented; no-op for now
  return () => {};
}
