import * as client from './client'

export async function getLogs(lines?: number): Promise<string[]> {
  return client.invoke<string[]>('get_logs', { lines: lines ?? 200 })
}

export async function clearLogs(): Promise<void> {
  return client.invoke('clear_logs')
}
