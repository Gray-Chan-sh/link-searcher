import * as client from './client'

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
    return client.invoke('trigger_backup')
}

export async function getBackupStatus(): Promise<BackupStatus> {
    return client.invoke<BackupStatus>('get_backup_status')
}

export async function listBackups(): Promise<BackupSnapshot[]> {
    return client.invoke<BackupSnapshot[]>('list_backups')
}

export async function exportBackup(destPath: string, password?: string | null, backupName?: string | null): Promise<BackupExportResult> {
    return client.invoke<BackupExportResult>('export_backup', { destPath, password: password ?? null, backupName: backupName ?? null })
}

export async function restoreBackup(backupName: string): Promise<void> {
    return client.invoke('restore_backup', { backupName })
}

export async function deleteBackup(backupName: string): Promise<void> {
    return client.invoke('delete_backup', { backupName })
}

export async function restoreFromZip(zipPath: string, password?: string | null): Promise<void> {
    return client.invoke('restore_from_zip', { zipPath, password: password ?? null })
}

export async function getDeadDirs(): Promise<DeadDir[]> {
    return client.invoke<DeadDir[]>('get_dead_dirs')
}

export async function remapDir(dirId: string, newPath: string): Promise<void> {
    return client.invoke('remap_dir', { dirId, newPath })
}

export async function removeDirWithFiles(dirId: string): Promise<void> {
    return client.invoke('remove_dir_with_files', { dirId })
}

// ───────────────────────── 恢复前目录映射（R1）─────────────────────────
// 旧流程「先整库覆盖、重启后按备份里的原机路径扫描」会在路径存在但内容不符时
// 静默改写/删除原记录。现在改为两步：plan（只读列出目录）→ 用户指定落位 → apply。

/** 备份里记录的一个监控目录。 */
export interface BackupDirInfo {
    id: string
    originalPath: string
    alias: string | null
    fileCount: number
}

/** `plan_restore` 的结果：目录清单 + 暂存 token。 */
export interface RestorePlan {
    token: string
    dirs: BackupDirInfo[]
    hasChatHistory: boolean
    hasConfig: boolean
}

/** 某个目录改指本机路径时的命中情况（只读预检）。 */
export interface RemapPreview {
    tracked: number
    found: number
    sizeMatch: number
    sampleMissing: string[]
}

export type DirActionKind = 'remap' | 'keep' | 'remove'

/** 恢复前，为备份里每个监控目录指定的落位方式（camelCase 对应后端 serde）。 */
export interface DirMapping {
    dirId: string
    action: DirActionKind
    newPath: string | null
}

/** 只读阶段：列出备份里的监控目录（zip 只解压到临时目录，不碰当前数据）。 */
export async function planRestore(source: 'zip' | 'snapshot', path: string, password?: string | null): Promise<RestorePlan> {
    return client.invoke<RestorePlan>('plan_restore', { source, path, password: password ?? null })
}

/** 只读预检：某目录改指 `newPath` 时能对上多少已索引文件。 */
export async function previewRemap(dirId: string, newPath: string, token?: string | null): Promise<RemapPreview> {
    return client.invoke<RemapPreview>('preview_remap', { token: token ?? null, dirId, newPath })
}

/** 执行阶段：按映射把恢复灌回当前数据并重启应用。 */
export async function applyRestore(token: string, mappings: DirMapping[], acknowledgeRisks: boolean): Promise<void> {
    return client.invoke('apply_restore', { token, mappings, acknowledgeRisks })
}