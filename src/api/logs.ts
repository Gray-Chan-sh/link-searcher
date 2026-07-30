import { invoke } from '@tauri-apps/api/core'

export async function getLogs(lines?: number): Promise<string[]> {
  return invoke<string[]>('get_logs', { lines: lines ?? 200 })
}

export async function clearLogs(): Promise<void> {
  return invoke('clear_logs')
}
