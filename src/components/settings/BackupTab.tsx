import { useI18n } from '../../i18n'
import { LoadingSpinner } from '../../icons'
import type { BackupStatus, BackupSnapshot, DeadDir } from '../../api/backup'
import { Section, formatSize } from './SettingsFields'

interface BackupTabProps {
  backupStatus: BackupStatus | null
  backingUp: boolean
  exporting: boolean
  exportPassword: string
  deadDirs: DeadDir[]
  backups: BackupSnapshot[]
  onBackupNow: () => void
  onExportBackup: (snapId?: string) => void
  onRestoreZip: () => void
  onRemapDir: (dirId: string) => void
  onRemoveDir: (dirId: string) => void
  onDeleteBackup: (snapId: string) => void
  onExportPasswordChange: (password: string) => void
}

export function BackupTab({ backupStatus, backingUp, exporting, exportPassword, deadDirs, backups, onBackupNow, onExportBackup, onRestoreZip, onRemapDir, onRemoveDir, onDeleteBackup, onExportPasswordChange }: BackupTabProps) {
  const { t } = useI18n()

  return (
    <div className="space-y-6">
      <Section title={t('backup_actions')}>
        <div className="space-y-2 text-sm text-gray-700 dark:text-gray-300">
          {backupStatus ? (
            <>
              {backupStatus.last_backup !== null && (
                <p>{t('backup_status_last', { time: new Date(backupStatus.last_backup * 1000).toLocaleString() })}</p>
              )}
              {backupStatus.last_backup === null && <p className="text-gray-400 dark:text-gray-500">{t('backup_none')}</p>}
              <p>{t('backup_status_count', { n: String(backupStatus.backup_count) })}</p>
              <p>{t('backup_status_size', { size: formatSize(backupStatus.backup_size) })}</p>
            </>
          ) : (
            <p className="text-gray-400 dark:text-gray-500">{t('loading')}</p>
          )}
        </div>
        <div className="flex items-center gap-3 mt-3">
          <button
            onClick={onBackupNow}
            disabled={backingUp}
            className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg disabled:opacity-50 transition-colors"
          >
            {backingUp && <LoadingSpinner className="size-3" />}
            {backingUp ? t('backup_in_progress') : t('backup_now')}
          </button>
        </div>
        <div className="flex items-center gap-3 mt-3">
          <input
            type="password"
            value={exportPassword}
            onChange={e => onExportPasswordChange(e.target.value)}
            placeholder={t('backup_export_password_placeholder')}
            className="flex-1 px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
          />
          <button
            onClick={() => onExportBackup()}
            disabled={backingUp || exporting}
            className="shrink-0 flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed rounded-lg transition-colors"
          >
            {(backingUp || exporting) && <LoadingSpinner className="size-3" />}
            {exporting ? t('backup_exporting') : t('backup_export_now')}
          </button>
        </div>
        <p className="mt-1.5 text-xs text-gray-400 dark:text-gray-500">{t('backup_export_current_hint')}</p>
        <div className="mt-3">
          <button
            onClick={onRestoreZip}
            className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-amber-600 dark:text-amber-400 bg-amber-50 dark:bg-amber-900/30 rounded-lg hover:bg-amber-100 dark:hover:bg-amber-900/50 transition-colors"
          >
            {t('backup_restore_zip')}
          </button>
        </div>
      </Section>

      {deadDirs.length > 0 && (
        <Section title={t('backup_dead_dirs')}>
          <p className="text-xs text-gray-500 dark:text-gray-400 mb-2">{t('backup_dead_dirs_desc')}</p>
          <div className="space-y-2">
            {deadDirs.map(d => (
              <div key={d.id} className="flex items-center gap-3 p-2 bg-gray-50 dark:bg-gray-800/40 border border-gray-200 dark:border-gray-700 rounded-lg">
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-mono text-gray-900 dark:text-gray-100 truncate">{d.path}</p>
                  <p className="text-xs text-gray-500 dark:text-gray-400">{t('backup_dead_dir_files', { n: String(d.file_count) })}</p>
                </div>
                <button
                  onClick={() => onRemapDir(d.id)}
                  className="px-2 py-1 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
                >
                  {t('backup_remap')}
                </button>
                <button
                  onClick={() => onRemoveDir(d.id)}
                  className="px-2 py-1 text-xs font-medium text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 rounded hover:bg-red-100 dark:hover:bg-red-900/40 transition-colors"
                >
                  {t('backup_remove')}
                </button>
              </div>
            ))}
          </div>
        </Section>
      )}

      {backups.length > 0 && (
        <Section title={t('backup_list')}>
          <div className="space-y-1.5">
            {[...backups].reverse().map(snap => (
              <div key={snap.id} className="flex items-center gap-2 px-2 py-1.5 rounded text-xs bg-gray-50 dark:bg-gray-800/40 border border-gray-200 dark:border-gray-700">
                <span className={`shrink-0 w-2 h-2 rounded-full ${snap.kind === 'baseline' ? 'bg-green-500' : 'bg-blue-400'}`} />
                <span className="font-mono text-gray-900 dark:text-gray-100 flex-1 truncate">{snap.id}</span>
                <span className="text-gray-500 dark:text-gray-400 shrink-0">{new Date(snap.ts * 1000).toLocaleDateString()} {new Date(snap.ts * 1000).toLocaleTimeString()}</span>
                <span className="text-gray-400 dark:text-gray-500 shrink-0">{formatSize(snap.size)}</span>
                <button
                  onClick={() => onExportBackup(snap.id)}
                  className="px-2 py-1 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
                >
                  {t('backup_export')}
                </button>
                <button
                  onClick={() => onDeleteBackup(snap.id)}
                  className="px-2 py-1 text-xs font-medium text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 rounded hover:bg-red-100 dark:hover:bg-red-900/40 transition-colors"
                >
                  {t('backup_delete')}
                </button>
              </div>
            ))}
          </div>
        </Section>
      )}
    </div>
  )
}
