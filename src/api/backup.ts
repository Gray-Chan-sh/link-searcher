import { invoke } from '@tauri-apps/api/core'

/** 备份/恢复相关 IPC 封装。has_secrets=true 表示未设置密码，前端需提示明文 API Key 风险。 */
export interface BackupStatus {
    last_backup: number | null
    backup_size: number
    backup_count: number
}

export interface BackupSnapshot {
    id: string
    ts: number
    kind: 'baseline' | 'incremental'
    size: number
}

export interface BackupExportResult {
    has_secrets: boolean
    dest_path: string
}

export interface DeadDir {
    id: string
    path: string
    file_count: number
}

export async function triggerBackup(): Promise<void> {
    return invoke('trigger_backup')
}

export async function getBackupStatus(): Promise<BackupStatus> {
    return invoke<BackupStatus>('get_backup_status')
}

export async function listBackups(): Promise<BackupSnapshot[]> {
    return invoke<BackupSnapshot[]>('list_backups')
}

export async function exportBackup(destPath: string, password?: string | null, backupName?: string | null): Promise<BackupExportResult> {
    return invoke<BackupExportResult>('export_backup', { destPath, password: password ?? null, backupName: backupName ?? null })
}

export async function restoreBackup(backupName: string): Promise<void> {
    return invoke('restore_backup', { backupName })
}

export async function restoreFromZip(zipPath: string, password?: string | null): Promise<void> {
    return invoke('restore_from_zip', { zipPath, password: password ?? null })
}

export async function getDeadDirs(): Promise<DeadDir[]> {
    return invoke<DeadDir[]>('get_dead_dirs')
}

export async function remapDir(dirId: string, newPath: string): Promise<void> {
    return invoke('remap_dir', { dirId, newPath })
}

export async function removeDirWithFiles(dirId: string): Promise<void> {
    return invoke('remove_dir_with_files', { dirId })
}