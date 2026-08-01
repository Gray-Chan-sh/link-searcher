import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export interface IndexStatus {
  total_files: number
  indexed: number
  pending: number
  errors: number
  ocred: number
  total_images: number
  last_scan: number | null
  is_scanning: boolean
  scan_delta?: { added: number; deleted: number; modified: number; errors: number }
}

export async function getIndexStatus(): Promise<IndexStatus> {
  return invoke<IndexStatus>('get_index_status')
}

export async function triggerScan(dirId?: string): Promise<void> {
  return invoke('trigger_scan', { dirId: dirId ?? null })
}

export async function rebuildIndex(): Promise<void> {
  return invoke('rebuild_index')
}

export async function cancelScan(): Promise<void> {
  return invoke('cancel_scan')
}

export interface IndexError {
  file_id: string
  file_path: string
  error_type: string
  error_msg: string
  created_at: number
}

export async function getIndexErrors(limit?: number): Promise<IndexError[]> {
    return invoke<IndexError[]>('get_index_errors', { limit: limit ?? 50 })
}

export interface ScanProgress {
    processed: number
    total: number
    current_file: string
    dir_id: string
    phase?: string
}

export async function listenScanProgress(callback: (progress: ScanProgress) => void): Promise<() => void> {
    return listen<ScanProgress>('scan-progress', (event) => {
        callback(event.payload)
    })
}
