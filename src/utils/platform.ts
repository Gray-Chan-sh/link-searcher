// Platform detection and browser-compatible abstractions for Tauri-specific APIs.
// Used by the API client layer to switch between Tauri IPC and HTTP fetch.

let _detected: boolean | null = null;

/** Detect if running inside Tauri (not a plain browser). */
export function isTauri(): boolean {
  if (_detected !== null) return _detected;
  _detected = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
  return _detected;
}

/** Get the Web API base URL (in browser mode, from window.location.origin). */
export function getApiBase(): string {
  return typeof window !== 'undefined' ? window.location.origin : '';
}

/** Get the saved Bearer token from localStorage or URL parameter. */
export function getToken(): string {
  if (typeof window === 'undefined') return '';
  return localStorage.getItem('ls_token') || '';
}

/** Save Bearer token to localStorage. */
export function setToken(token: string): void {
  if (typeof window === 'undefined') return;
  localStorage.setItem('ls_token', token);
}

/** Convert a Tauri asset:// path to a loadable URL. In browser mode, routes through the API. */
export async function resolveAssetUrl(fileId: string): Promise<string> {
  if (isTauri()) {
    const { convertFileSrc } = await import('@tauri-apps/api/core');
    return convertFileSrc(fileId);
  }
  return `${getApiBase()}/api/files/${fileId}/preview`;
}

/** Synchronous version for JSX src attributes. */
export function resolveAssetUrlSync(fileId: string): string {
  if (isTauri()) {
    // ponytail: asset:// protocol works in Tauri without async convertFileSrc
    return `asset://localhost/${fileId}`;
  }
  return `${getApiBase()}/api/files/${fileId}/preview`;
}

/** Platform-agnostic confirm dialog. */
export async function confirm(message: string, title?: string): Promise<boolean> {
  if (isTauri()) {
    const { ask } = await import('@tauri-apps/plugin-dialog');
    return ask(message, { title: title || '确认' });
  }
  return window.confirm(message);
}

/** Platform-agnostic alert dialog. */
export async function alert(message: string, title?: string): Promise<void> {
  if (isTauri()) {
    const { message: msg } = await import('@tauri-apps/plugin-dialog');
    await msg(message, { title: title || '提示' });
    return;
  }
  window.alert(message);
}

/** Platform-agnostic save file dialog. Returns the path or downloads the content in browser. */
export async function saveFile(content: string, defaultName: string): Promise<void> {
  if (isTauri()) {
    const { save } = await import('@tauri-apps/plugin-dialog');
    const { writeTextFile } = await import('@tauri-apps/plugin-fs');
    const path = await save({ defaultPath: defaultName, filters: [{ name: 'All Files', extensions: ['*'] }] });
    if (path) await writeTextFile(path, content);
  } else {
    const blob = new Blob([content], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = defaultName;
    a.click();
    URL.revokeObjectURL(url);
  }
}

/** Platform-agnostic export file dialog (returns path, or downloads in browser). */
export async function exportFile(content: string, defaultName: string, mimeType: string): Promise<void> {
  if (isTauri()) {
    const { save } = await import('@tauri-apps/plugin-dialog');
    const { writeTextFile } = await import('@tauri-apps/plugin-fs');
    const ext = defaultName.split('.').pop() || '*';
    const path = await save({ defaultPath: defaultName, filters: [{ name: ext.toUpperCase(), extensions: [ext] }] });
    if (path) await writeTextFile(path, content);
  } else {
    const blob = new Blob([content], { type: mimeType });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = defaultName;
    a.click();
    URL.revokeObjectURL(url);
  }
}

/** Open a directory picker. In browser mode, returns null (not supported). */
export async function openDirectory(): Promise<string | null> {
  if (isTauri()) {
    const { open } = await import('@tauri-apps/plugin-dialog');
    const dir = await open({ directory: true, multiple: false });
    return dir as string | null;
  }
  return null;
}