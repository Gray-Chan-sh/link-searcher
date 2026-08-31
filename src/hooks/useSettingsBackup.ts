import { useState, useEffect } from 'react'
import { useI18n } from '../i18n'
import { confirm, alert, openDirectory } from '../utils/platform'
import { triggerBackup, getBackupStatus, exportBackup, listBackups, restoreFromZip, deleteBackup, getDeadDirs, remapDir, removeDirWithFiles, type BackupStatus, type BackupSnapshot, type DeadDir } from '../api/backup'

export function useSettingsBackup() {
  const { t } = useI18n()
  const [backupStatus, setBackupStatus] = useState<BackupStatus | null>(null)
  const [backingUp, setBackingUp] = useState(false)
  const [exporting, setExporting] = useState(false)
  const [exportPassword, setExportPassword] = useState('')
  const [deadDirs, setDeadDirs] = useState<DeadDir[]>([])
  const [backups, setBackups] = useState<BackupSnapshot[]>([])

  useEffect(() => {
    getBackupStatus().then(setBackupStatus).catch(() => {})
    listBackups().then(setBackups).catch(() => {})
    getDeadDirs().then(setDeadDirs).catch(() => {})
    const id = setInterval(() => { getBackupStatus().then(setBackupStatus).catch(() => {}); listBackups().then(setBackups).catch(() => {}) }, 60_000)
    return () => clearInterval(id)
  }, [])

  const handleBackupNow = async () => {
    setBackingUp(true)
    try {
      await triggerBackup()
      const [status, snaps] = await Promise.all([getBackupStatus(), listBackups()])
      setBackupStatus(status)
      setBackups(snaps)
    } catch (e) {
      await alert(e instanceof Error ? e.message : String(e), t('backup_failed'))
    } finally {
      setBackingUp(false)
    }
  }

  const handleExportBackup = async (snapId?: string) => {
    setExporting(true)
    try {
      const result = await exportBackup(snapId ?? '', exportPassword || undefined)
      await alert(t('backup_export_done', { path: result.dest_path }), t('backup_export'))
    } catch (e) {
      await alert(e instanceof Error ? e.message : String(e), t('backup_export_failed'))
    } finally {
      setExporting(false)
    }
  }

  const handleRestoreZip = async () => {
    const dir = await openDirectory()
    if (!dir) return
    const confirmed = await confirm(t('backup_restore_confirm'), t('backup_restore'))
    if (!confirmed) return
    try {
      await restoreFromZip(dir)
      await alert(t('backup_restore_done'), t('backup_restore'))
    } catch (e) {
      await alert(e instanceof Error ? e.message : String(e), t('backup_restore_failed'))
    }
  }

  const handleRemapDir = async (dirId: string) => {
    const newPath = await openDirectory()
    if (!newPath) return
    try {
      await remapDir(dirId, newPath)
      const [snaps, dead] = await Promise.all([listBackups(), getDeadDirs()])
      setBackups(snaps)
      setDeadDirs(dead)
    } catch (e) {
      await alert(e instanceof Error ? e.message : String(e), t('backup_remap_failed'))
    }
  }

  const handleRemoveDir = async (dirId: string) => {
    const confirmed = await confirm(t('backup_remove_confirm'), t('backup_remove'))
    if (!confirmed) return
    try {
      await removeDirWithFiles(dirId)
      const [snaps, dead] = await Promise.all([listBackups(), getDeadDirs()])
      setBackups(snaps)
      setDeadDirs(dead)
    } catch (e) {
      await alert(e instanceof Error ? e.message : String(e), t('backup_remove_failed'))
    }
  }

  const handleDeleteBackup = async (snapId: string) => {
    const confirmed = await confirm(t('backup_delete_confirm'), t('backup_delete'))
    if (!confirmed) return
    try {
      await deleteBackup(snapId)
      const [status, snaps] = await Promise.all([getBackupStatus(), listBackups()])
      setBackupStatus(status)
      setBackups(snaps)
    } catch (e) {
      await alert(e instanceof Error ? e.message : String(e), t('backup_delete_failed'))
    }
  }

  return {
    backupStatus, backingUp, exporting, exportPassword, setExportPassword,
    deadDirs, backups,
    handleBackupNow, handleExportBackup, handleRestoreZip,
    handleRemapDir, handleRemoveDir, handleDeleteBackup,
  }
}
