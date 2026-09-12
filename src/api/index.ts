import * as client from './client'

export interface TaskBrief {
  task: string
  summary: string
  completed_at: number
}

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
  running_tasks: string[]
  briefs: TaskBrief[]
}

export async function getIndexStatus(): Promise<IndexStatus> {
  return client.invoke<IndexStatus>('get_index_status')
}

export interface BackfillReport {
  processed: number
  pending: number
  failed: number
}

export async function backfillEmbeddings(): Promise<BackfillReport> {
  return client.invoke<BackfillReport>('backfill_embeddings')
}

export interface VerifyReport {
  checked: number
  recovered: number
  dead: number
  failed: number
}

export async function verifyIndexContent(forceDead: boolean): Promise<VerifyReport> {
  return client.invoke<VerifyReport>('verify_index_content', { forceDead })
}

export interface ReextractReport {
  processed: number
  ok: number
  failed: number
  /** Only present from quality-framework re-extract commands */
  exhausted?: number
}

export async function reextractMissingContent(limit?: number): Promise<ReextractReport> {
  return client.invoke<ReextractReport>('reextract_missing_content', { limit: limit ?? 500 })
}

export async function triggerScan(dirId?: string): Promise<void> {
  return client.invoke('trigger_scan', { dirId: dirId ?? null })
}

export async function rebuildIndex(): Promise<void> {
  return client.invoke('rebuild_index')
}

export async function reindexFile(fileId: string): Promise<void> {
  return client.invoke('reindex_file', { fileId })
}

export async function reindexFiles(ids: string[]): Promise<ReextractReport> {
  return client.invoke<ReextractReport>('reindex_files', { fileIds: ids })
}

// ── Quality framework ──

export interface QualitySummary {
  green: number
  yellow: number
  red: number
  unevaluated: number
  total: number
}

export async function getQualitySummary(): Promise<QualitySummary> {
  return client.invoke<QualitySummary>('get_quality_summary')
}

export interface QualityAuditEntry {
  md5: string
  file_id: string | null
  file_path: string | null
  quality_score: number | null
  quality_flags: string
  reextract_count: number
  char_count: number
  ocr_used: boolean
  preview: string
}

export async function qualityAudit(minScore?: number, limit?: number): Promise<QualityAuditEntry[]> {
  return client.invoke<QualityAuditEntry[]>('quality_audit', {
    minScore: minScore ?? null,
    limit: limit ?? 100,
  })
}

export interface ReextractOutcome {
  reextracted: boolean
  old_score: number | null
  new_score: number | null
  reason: string | null
}

export async function reExtractFile(fileId: string, engine?: string): Promise<ReextractOutcome> {
  return client.invoke<ReextractOutcome>('re_extract_file', {
    fileId,
    engine: engine ?? null,
  })
}

export async function reExtractLowQuality(limit?: number): Promise<ReextractReport> {
  return client.invoke<ReextractReport>('re_extract_low_quality', {
    limit: limit ?? 50,
  })
}

/** Restore soft-deleted records (Browse「已删除」视图) and re-index them.
 *  Returns the number of files actually restored (disk-present ones). */
export async function restoreFiles(ids: string[]): Promise<number> {
  return client.invoke<number>('restore_files', { ids })
}

export async function cancelScan(): Promise<void> {
  return client.invoke('cancel_scan')
}

export interface IndexError {
  file_id: string
  file_path: string
  error_type: string
  error_msg: string
  created_at: number
}

export async function getIndexErrors(limit?: number): Promise<IndexError[]> {
    return client.invoke<IndexError[]>('get_index_errors', { limit: limit ?? 50 })
}

export interface ScanProgress {
    processed: number
    total: number
    current_file: string
    dir_id: string
    phase?: string
}

export async function listenScanProgress(callback: (progress: ScanProgress) => void): Promise<() => void> {
    return client.listen<ScanProgress>('scan-progress', (event) => {
        callback(event)
    })
}
