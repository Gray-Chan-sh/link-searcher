// Unified API client: routes invoke calls to Tauri IPC or HTTP fetch automatically.
import { isTauri, getApiBase, getToken } from '../utils/platform';

export type InvokeArgs = Record<string, unknown>;

interface HttpSpec {
  method: string;
  path?: string;
  body?: unknown;
  /** Server responds with an SSE stream that only *triggers* work; results arrive via the /api/events bridge. */
  sse?: boolean;
  transform?: (data: unknown, args: InvokeArgs) => unknown;
  paramMap?: Record<string, string>;
  dynamicPath?: (args: InvokeArgs) => string;
}

type Mapping = HttpSpec | ((args: InvokeArgs) => HttpSpec);

function buildQuery(args: InvokeArgs, paramMap?: Record<string, string>): string {
  const parts: string[] = [];
  for (const [k, v] of Object.entries(args)) {
    if (v === undefined || v === null) continue;
    if (k === 'scope' || k === 'session' || typeof v === 'object') continue;
    const mappedKey = paramMap?.[k] ?? k;
    if (Array.isArray(v)) {
      if (v.length > 0) parts.push(`${mappedKey}=${v.join(',')}`);
    } else {
      parts.push(`${mappedKey}=${encodeURIComponent(String(v))}`);
    }
  }
  return parts.length ? '?' + parts.join('&') : '';
}

// Command → HTTP endpoint mapping with response transformation.
// Mirrors src-tauri/src/webapi/routes/*.rs.
const MAPPINGS: Record<string, Mapping> = {
  // ── Search ──
  search: {
    method: 'GET', path: '/api/search',
    paramMap: { query: 'q', pageSize: 'page_size', dirIds: 'dir_ids', extFilter: 'ext_filter' },
  },
  suggest: { method: 'GET', path: '/api/suggest' },
  search_file_paths: { method: 'GET', path: '/api/search/paths' },
  search_tree_prune: { method: 'GET', path: '/api/search/tree-prune' },
  get_search_history: { method: 'GET', path: '/api/search/history' },
  clear_search_history: { method: 'DELETE', path: '/api/search/history' },
  // ExportBody on the wire is snake_case (no serde rename on the server).
  export_search_results: (a) => ({
    method: 'POST', path: '/api/search/export',
    body: { query: a.query, dir_ids: a.dirIds, ext_filter: a.extFilter, format: a.format },
  }),
  get_file_type_stats: { method: 'GET', path: '/api/stats/file-types' },
  get_browse_file_types: { method: 'GET', path: '/api/stats/browse-types' },

  // ── Files ──
  list_files_db: { method: 'GET', path: '/api/files' },
  list_files: { method: 'GET', path: '/api/files/browse' },
  list_dir_entries: { method: 'GET', path: '/api/dir-entries' },
  get_file: (a) => ({ method: 'GET', path: `/api/files/${a.id}` }),
  get_duplicates: { method: 'GET', path: '/api/files', transform: () => [] },
  preview_file: (a) => ({ method: 'GET', path: `/api/files/${a.id}/preview` }),
  get_file_preview: (a) => ({ method: 'GET', path: `/api/files/${a.id}/preview` }),
  preview_file_by_path: { method: 'GET', path: '/api/files/preview-by-path' },
  download_files: { method: 'POST', path: '/api/files/download' },
  open_file: { method: 'POST', path: '/api/files/open' },
  reveal_in_folder: { method: 'POST', path: '/api/files/reveal' },

  // ── Index ──
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
  // Server reads body field "file_id" (snake_case).
  reindex_file: (a) => ({ method: 'POST', path: '/api/reindex', body: { file_id: a.fileId } }),
  rebuild_index: { method: 'POST', path: '/api/index/rebuild' },
  reindex_files: (a) => ({ method: 'POST', path: '/api/index/reindex-batch', body: a }),
  reextract_missing_content: (a) => ({ method: 'POST', path: '/api/index/reextract', body: a }),
  verify_index_content: (a) => ({ method: 'POST', path: '/api/index/verify', body: a }),
  get_index_errors: { method: 'GET', path: '/api/index/errors' },
  check_index_integrity: { method: 'GET', path: '/api/index/integrity' },
  backfill_embeddings: { method: 'POST', path: '/api/index/backfill-embeddings' },

  // ── Dirs ──
  list_dirs: {
    method: 'GET', path: '/api/dirs',
    transform: (data) => (data as Record<string, unknown>)?.dirs ?? data ?? [],
  },
  add_dir: (a) => ({ method: 'POST', path: '/api/dirs', body: a }),
  remove_dir: (a) => ({ method: 'DELETE', path: '/api/dirs', body: { id: a.id } }),
  update_dir: (a) => ({ method: 'PUT', path: '/api/dirs', body: a }),
  get_dir_tree: { method: 'GET', path: '/api/dirs/tree' },
  get_dir_children: { method: 'GET', path: '/api/dirs/children' },

  // ── AI ──
  ai_capabilities: { method: 'GET', path: '/api/ai/capabilities' },
  ask_documents: (a) => ({ method: 'POST', path: '/api/chat/ask', body: a }),
  summarize_file: (a) => ({ method: 'POST', path: '/api/ai/summarize', body: a }),
  smart_search: (a) => ({ method: 'POST', path: '/api/ai/smart-search', body: a }),
  conversation_ask: (a) => ({ method: 'POST', path: '/api/ai/conversation/ask', body: a }),
  // Stream variants trigger the work server-side and answer with an SSE frame
  // stream; chunks are delivered through the shared /api/events bridge, so we
  // fire the request and close the direct connection (see `sse` in invoke).
  smart_search_stream: (a) => ({ method: 'POST', path: '/api/ai/smart-search/stream', body: a, sse: true }),
  conversation_ask_stream: (a) => ({ method: 'POST', path: '/api/ai/conversation/ask/stream', body: a, sse: true }),
  test_ai_gateway: { method: 'GET', path: '/api/ai/gateways/test' },
  cancel_ai_request: { method: 'POST', path: '/api/ai/cancel' },

  // ── Chat sessions ──
  list_chat_sessions: {
    method: 'GET', path: '/api/chat/sessions',
    transform: (data) => (data as Record<string, unknown>)?.sessions ?? data ?? [],
  },
  create_chat_session: { method: 'POST', path: '/api/chat/sessions', transform: (data) => (data as Record<string, unknown>)?.id },
  load_chat_session: (a) => ({ method: 'GET', path: `/api/chat/sessions/${a.id}` }),
  delete_chat_session: (a) => ({ method: 'DELETE', path: `/api/chat/sessions/${a.id}` }),
  save_chat_session: (a) => ({
    method: 'PUT',
    path: `/api/chat/sessions/${(a.session as Record<string, unknown> | undefined)?.id ?? a.id}`,
    body: a,
  }),
  export_chat_session: (a) => ({
    method: 'POST', path: `/api/chat/sessions/${a.id}/export`,
    transform: (data) => (data as Record<string, unknown>)?.markdown,
  }),
  export_chat_session_json: (a) => ({
    method: 'POST', path: `/api/chat/sessions/${a.id}/export`,
    transform: (data) => (data as Record<string, unknown>)?.json,
  }),

  // ── Settings ──
  get_settings: { method: 'GET', path: '/api/settings' },
  update_settings: (a) => ({ method: 'PUT', path: '/api/settings', body: a }),
  update_token: (a) => ({ method: 'POST', path: '/api/auth/token', body: a }),
  get_version: { method: 'GET', path: '/api/version' },

  // ── Config / providers ──
  get_config: { method: 'GET', path: '/api/config' },
  // Server takes the ConfigInfo value directly, frontend wraps it as newConfig.
  update_config: (a) => ({ method: 'PUT', path: '/api/config', body: a.newConfig ?? a }),
  migrate_data: (a) => ({ method: 'POST', path: '/api/config/migrate', body: a }),
  restart_app: { method: 'POST', path: '/api/system/restart' },
  add_provider: (a) => ({ method: 'POST', path: '/api/config/providers', body: a }),
  update_provider: (a) => ({ method: 'PUT', path: `/api/config/providers/${a.id}`, body: a }),
  delete_provider: (a) => ({ method: 'DELETE', path: `/api/config/providers/${a.id}` }),
  refresh_provider_models: (a) => ({ method: 'POST', path: `/api/config/providers/${a.id}/refresh` }),
  set_active_model: (a) => ({ method: 'PUT', path: '/api/config/active-model', body: a }),
  // Server ignores the path id (Path(_id)); credentials travel in the body.
  test_provider: (a) => ({ method: 'POST', path: '/api/config/providers/-/test', body: a }),

  // ── Logs ──
  get_logs: { method: 'GET', path: '/api/logs' },
  clear_logs: { method: 'DELETE', path: '/api/logs' },
  list_session_logs: { method: 'GET', path: '/api/logs/sessions' },

  // ── Backup ──
  trigger_backup: { method: 'POST', path: '/api/backup/trigger' },
  get_backup_status: { method: 'GET', path: '/api/backup/status' },
  list_backups: { method: 'GET', path: '/api/backup/list' },
  export_backup: (a) => ({ method: 'POST', path: '/api/backup/export', body: a }),
  restore_from_zip: (a) => ({ method: 'POST', path: '/api/backup/restore-zip', body: a }),
  restore_backup: (a) => ({ method: 'POST', path: '/api/backup/restore', body: a }),
  delete_backup: (a) => ({ method: 'DELETE', path: `/api/backup/${encodeURIComponent(String(a.backupName))}` }),
  get_dead_dirs: { method: 'GET', path: '/api/backup/dead-dirs' },
  remap_dir: (a) => ({ method: 'POST', path: '/api/backup/remap', body: a }),
  remove_dir_with_files: (a) => ({ method: 'DELETE', path: `/api/backup/dir/${a.dirId}` }),

  // ── OCR ──
  check_tesseract: { method: 'GET', path: '/api/ocr/tesseract' },
  list_ocr_engines: { method: 'GET', path: '/api/ocr/engines' },
  test_ocr_engine: (a) => ({ method: 'POST', path: '/api/ocr/engines/test', body: a }),
  check_dependencies: { method: 'GET', path: '/api/ocr/dependencies' },
  get_file_type_support: { method: 'GET', path: '/api/ocr/file-type-support' },
  get_unsupported_ext_stats: { method: 'GET', path: '/api/ocr/unsupported-exts' },

  // ── Model installers ──
  check_bge_installed: { method: 'GET', path: '/api/ai/install/bge/status' },
  install_bge: (a) => ({ method: 'POST', path: '/api/ai/install/bge', body: a }),
  install_funasr: { method: 'POST', path: '/api/ai/install/funasr' },
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
  const hasBody = spec.body !== undefined && spec.body !== null;
  const qs = !hasBody && (spec.method === 'GET' || spec.method === 'DELETE')
    ? buildQuery(args || {}, spec.paramMap)
    : '';
  const url = getApiBase() + spec.path + qs;

  const headers: Record<string, string> = { Authorization: `Bearer ${getToken()}` };
  if (hasBody) headers['Content-Type'] = 'application/json';

  const resp = await fetch(url, {
    method: spec.method,
    headers,
    body: hasBody ? JSON.stringify(spec.body) : undefined,
  });

  if (resp.status === 401) {
    if (typeof window !== 'undefined') {
      localStorage.removeItem('ls_token')
      window.dispatchEvent(new Event('auth-failed'))
    }
    throw new Error('Token 无效，请重新输入')
  }
  if (!resp.ok) {
    const err = await resp.json().catch(() => ({ error: `HTTP ${resp.status}` }));
    throw new Error(err.error || `HTTP ${resp.status}`);
  }

  if (spec.sse) {
    // Work was detached server-side before this response; progress arrives via
    // listen() on /api/events. Close the per-request SSE stream immediately.
    await resp.body?.cancel();
    return undefined as T;
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

  // Web mode: consume the /api/events SSE bridge via fetch (EventSource cannot
  // send the Authorization header).
  const controller = new AbortController();
  const token = getToken();

  fetch(getApiBase() + '/api/events', {
    headers: { Authorization: `Bearer ${token}` },
    signal: controller.signal,
  }).then(async (resp) => {
    const reader = resp.body!.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });

      let idx;
      while ((idx = buffer.indexOf('\n\n')) >= 0) {
        const chunk = buffer.slice(0, idx);
        buffer = buffer.slice(idx + 2);

        let eventName = '', data = '';
        for (const line of chunk.split('\n')) {
          if (line.startsWith('event: ')) eventName = line.slice(7);
          if (line.startsWith('data: ')) data += (data ? '\n' : '') + line.slice(6);
        }

        if (eventName === event && data) {
          try {
            handler(JSON.parse(data));
          } catch {
            handler(data as unknown as T);
          }
        }
      }
    }
  }).catch(() => {}); // AbortError when unlistened

  return () => controller.abort();
}
