/**
 * Canonical in-app path form: forward slashes only.
 *
 * Windows sources (Tauri dialog, OS drag-drop, backend `read_dir`) carry `\`;
 * the DB/Tantivy layer stores `/`. Normalizing at every IPC boundary keeps
 * scope matching, mention chips and prefix comparisons separator-safe on
 * all platforms (on macOS/Linux the call is a no-op).
 */
export function normalizePath(p: string): string {
  return p.replace(/\\/g, '/')
}
