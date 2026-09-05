import { useEffect, useRef, useState } from 'react'
import { listen } from '../api/client'
import { confirm, alert, openDirectory } from '../utils/platform'
import { useSettings } from '../hooks/useSettings'
import { usePersistentState } from '../hooks/usePersistentState'
import { useTheme } from '../theme'
import { useI18n } from '../i18n'
import { LoadingSpinner } from '../icons'
import { getConfig, migrateData, restartApp, updateConfig, type ConfigInfo, type MigrationProgress, type MigrationWarning } from '../api/config'
import { checkBgeInstalled, getVersion, installBge, updateSettings } from '../api/settings'
import { useSettingsProviders } from '../hooks/useSettingsProviders'
import { useSettingsBackup } from '../hooks/useSettingsBackup'
import { useSettingsOcr } from '../hooks/useSettingsOcr'
import { GeneralTab, DocsTab, IndexTab, AiTab, DepsTab, BackupTab, SystemTab } from '../components/settings'

export default function Settings() {
  const { t, lang, setLang } = useI18n()
  const { settings, loading, error, setValue } = useSettings()
  const { theme, setTheme } = useTheme()
  const [activeTab, setActiveTab] = usePersistentState<string>('settings_tab', 'general')
  const [appConfig, setAppConfig] = useState<ConfigInfo | null>(null)
  const [migrating, setMigrating] = useState(false)
  const [migrationStage, setMigrationStage] = useState<string | null>(null)
  const [migrationProgress, setMigrationProgress] = useState(0)
  const [localError, setLocalError] = useState<string | null>(null)
  const [version, setVersion] = useState<{ hash: string; time: string } | null>(null)
  const [bgeInstalling, setBgeInstalling] = useState(false)
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  const ocr = useSettingsOcr()
  const backup = useSettingsBackup()
  const providers = useSettingsProviders(appConfig, setAppConfig)

  useEffect(() => {
    getVersion().then(setVersion).catch(() => {})
  }, [])

  useEffect(() => {
    getConfig().then(setAppConfig).catch(() => {})
  }, [])

  useEffect(() => {
    const unlisteners: (() => void)[] = []
    listen<MigrationProgress>('migration-progress', payload => {
      setMigrationProgress(payload.progress)
      setMigrationStage(payload.stage)
    }).then(u => unlisteners.push(u))
    listen<MigrationWarning>('migration-warning', payload => {
      void alert(payload.message, '迁移警告')
    }).then(u => unlisteners.push(u))
    listen<{ success: boolean; message: string }>('bge-install-done', async payload => {
      setBgeInstalling(false)
      checkBgeInstalled().then(ocr.setBgeStatus).catch(() => {})
      if (payload.message) {
        await alert(payload.message, 'BGE')
      }
    }).then(u => unlisteners.push(u))
    return () => {
      unlisteners.forEach(u => u())
    }
  }, [ocr])

  useEffect(() => {
    return () => {
      if (saveTimerRef.current) clearTimeout(saveTimerRef.current)
    }
  }, [])

  const handleChangeDataDir = async () => {
    const selected = await openDirectory()
    if (!selected || !appConfig) return
    if (selected === appConfig.data_dir) return

    setMigrating(true)
    setMigrationProgress(0)
    setMigrationStage('preparing')
    try {
      const msg = await migrateData(appConfig.data_dir, selected)
      setAppConfig({ ...appConfig, data_dir: selected })
      await updateConfig({ data_dir: selected })
      setLocalError(null)
      setMigrating(false)
      const restart = await confirm(msg, '迁移完成')
      if (restart) await restartApp()
    } catch (e: unknown) {
      const err = e instanceof Error ? e.message : String(e)
      setLocalError(`迁移失败: ${err}`)
      await alert(`迁移失败:\n${err}`, '迁移失败')
    } finally {
      setMigrating(false)
      setMigrationStage(null)
    }
  }

  const handleChangeLang = async (newLang: string) => {
    await setLang(newLang as 'zh' | 'en' | 'ja' | 'ko')
  }

  const handleFieldChange = (key: string, value: string) => {
    setValue(key, value)
    setLocalError(null)
    if (saveTimerRef.current) clearTimeout(saveTimerRef.current)
    saveTimerRef.current = setTimeout(() => {
      updateSettings({ [key]: value })
        .catch(e => setLocalError(e instanceof Error ? e.message : 'Failed to save setting'))
    }, 300)
  }

  const handleRegenerateToken = () => {
    const bytes = new Uint8Array(16)
    crypto.getRandomValues(bytes)
    handleFieldChange('web_api_token', Array.from(bytes, b => b.toString(16).padStart(2, '0')).join(''))
  }

  const selectedEngine = ocr.ocrEngines.find(e => e.engine_type === (settings['ocr_engine'] ?? 'PaddleOCR'))

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <LoadingSpinner className="size-6 text-blue-500" />
      </div>
    )
  }

  if (error) {
    return (
      <div className="h-full p-6">
        <div className="px-4 py-3 text-sm text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-900 rounded-lg">
          {error}
        </div>
      </div>
    )
  }

  return (
    <div className="h-full p-6 overflow-y-auto">
      <div className="flex items-center justify-between mb-6">
        <div>
          <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">{t('settings')}</h2>
          <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
            {t('settings_desc')}
          </p>
        </div>
      </div>

      {localError && (
        <div className="px-4 py-3 mb-4 text-sm text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-900 rounded-lg">
          {localError}
        </div>
      )}

      <div className="mb-6 flex gap-1 border-b border-gray-200 dark:border-gray-800">
        {[
          { id: 'general', key: t('tab_general') },
          { id: 'index', key: t('tab_index') },
          { id: 'deps', key: t('dep_center_title') },
          { id: 'docs', key: t('tab_docs') },
          { id: 'ai', key: t('tab_ai') },
          { id: 'backup', key: t('tab_backup') },
          { id: 'system', key: t('tab_system') },
        ].map(tab => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            className={`px-4 py-2 text-sm font-medium border-b-2 -mb-px transition-colors ${
              activeTab === tab.id
                ? 'text-blue-600 dark:text-blue-400 border-blue-500'
                : 'text-gray-500 dark:text-gray-400 border-transparent hover:text-gray-700 dark:hover:text-gray-300'
            }`}
          >
            {tab.key}
          </button>
        ))}
      </div>

      <div className="space-y-6 max-w-2xl">
        {activeTab === 'general' && (
          <GeneralTab
            appConfig={appConfig}
            migrating={migrating}
            migrationProgress={migrationProgress}
            migrationStage={migrationStage}
            lang={lang}
            theme={theme}
            onChangeLang={handleChangeLang}
            onChangeTheme={v => setTheme(v as 'light' | 'dark' | 'system')}
            onChangeDataDir={handleChangeDataDir}
          />
        )}

        {activeTab === 'docs' && <DocsTab />}

        {activeTab === 'deps' && <DepsTab />}

        {activeTab === 'index' && (
          <IndexTab
            ocrEngines={ocr.ocrEngines}
            ocrTesting={ocr.ocrTesting}
            ocrResult={ocr.ocrResult}
            selectedEngine={selectedEngine}
            ocrLang={settings['ocr_lang'] ?? 'eng'}
            onTestOcr={() => ocr.handleTestOcr(settings['ocr_engine'] ?? 'PaddleOCR')}
            onChangeOcrEngine={engineType => handleFieldChange('ocr_engine', engineType)}
            onChangeOcrLang={v => handleFieldChange('ocr_lang', v)}
          />
        )}

        {activeTab === 'ai' && (
          <AiTab
            appConfig={appConfig}
            setAppConfig={setAppConfig}
            caps={providers.caps}
            aiWarn={providers.aiWarn}
            bgeStatus={ocr.bgeStatus}
            bgeInstalling={bgeInstalling}
            editingId={providers.editingId}
            editDraft={providers.editDraft}
            savingId={providers.savingId}
            testingId={providers.testingId}
            testOutcome={providers.testOutcome}
            refreshingId={providers.refreshingId}
            refreshMsg={providers.refreshMsg}
            adding={providers.adding}
            newProv={providers.newProv}
            modelFilter={providers.modelFilter}
            expandedGroups={providers.expandedGroups}
            aiTest={providers.aiTest}
            aiTestLoading={providers.aiTestLoading}
            onSaveSemanticWeight={providers.handleSaveSemanticWeight}
            onActiveModel={providers.handleActiveModel}
            onTestProvider={providers.handleTestProvider}
            onRefreshProvider={providers.handleRefreshProvider}
            onDeleteProvider={providers.handleDeleteProvider}
            onOpenEdit={providers.openEdit}
            onSaveEdit={providers.handleSaveEdit}
            onModelType={providers.handleModelType}
            onToggleEnabled={providers.handleToggleEnabled}
            onAddProvider={providers.handleAddProvider}
            onTestAi={providers.testAi}
            onInstallBge={() => {
              setBgeInstalling(true)
              installBge().catch(e => {
                setBgeInstalling(false)
                setLocalError(e instanceof Error ? e.message : String(e))
              })
            }}
            providerInUse={providers.providerInUse}
            modelInUse={providers.modelInUse}
            modelOptions={kind => providers.modelOptions(kind, ocr.bgeStatus)}
            setEditingId={providers.setEditingId}
            setEditDraft={providers.setEditDraft}
            setAdding={providers.setAdding}
            setNewProv={providers.setNewProv}
            setModelFilter={providers.setModelFilter}
            setExpandedGroups={providers.setExpandedGroups}
          />
        )}

        {activeTab === 'backup' && (
          <BackupTab
            backupStatus={backup.backupStatus}
            backingUp={backup.backingUp}
            exporting={backup.exporting}
            exportPassword={backup.exportPassword}
            deadDirs={backup.deadDirs}
            backups={backup.backups}
            onBackupNow={backup.handleBackupNow}
            onExportBackup={backup.handleExportBackup}
            onRestoreZip={backup.handleRestoreZip}
            onRemapDir={backup.handleRemapDir}
            onRemoveDir={backup.handleRemoveDir}
            onDeleteBackup={backup.handleDeleteBackup}
            onExportPasswordChange={backup.setExportPassword}
          />
        )}

        {activeTab === 'system' && (
          <SystemTab
            settings={settings}
            onFieldChange={handleFieldChange}
            onRegenerateToken={handleRegenerateToken}
          />
        )}
      </div>

      {version && (
        <div className="text-center text-xs text-gray-400 dark:text-gray-600 mt-4 pt-4 border-t border-gray-100 dark:border-gray-800">
          git: {version.hash} ({version.time})
        </div>
      )}
    </div>
  )
}
