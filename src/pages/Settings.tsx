import { useCallback, useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { ask, open, message, save } from '@tauri-apps/plugin-dialog'
import { listen } from '@tauri-apps/api/event'
import { useSettings } from '../hooks/useSettings'
import { usePersistentState } from '../hooks/usePersistentState'
import { useTheme } from '../theme'
import { useI18n } from '../i18n'
import { LoadingSpinner, PlusIcon } from '../icons'
import { addProvider, deleteProvider, getConfig, migrateData, refreshProviderModels, restartApp, setActiveModel, testProvider, updateConfig, type ConfigInfo, type MigrationProgress, type MigrationWarning, type ModelType, type ProviderInfo } from '../api/config'
import { aiCapabilities, type AiCapabilities } from '../api/files'
import { checkDependencies, checkBgeInstalled, getVersion, installBge, installFunasr, listOcrEngines, testOcrEngine, updateSettings, type BgeStatus, type DependencyStatus, type OcrEngineStatus, type OcrTestResult, type FunasrInstallResult } from '../api/settings'
import { triggerBackup, getBackupStatus, exportBackup, restoreBackup, listBackups, restoreFromZip, deleteBackup, getDeadDirs, remapDir, removeDirWithFiles, type BackupStatus, type BackupSnapshot, type DeadDir } from '../api/backup'

const OCR_LANGS = [
  { value: 'eng', label: 'English' },
  { value: 'chi_sim', label: 'Chinese (Simplified)' },
  { value: 'jpn', label: 'Japanese' },
  { value: 'kor', label: 'Korean' },
]

const LANG_OPTIONS = [
  { value: 'zh', labelKey: 'chinese' },
  { value: 'en', labelKey: 'english' },
  { value: 'ja', labelKey: 'japanese' },
  { value: 'ko', labelKey: 'korean' },
]

export default function Settings() {
  const { t, lang, setLang } = useI18n()
  const { settings, loading, error, setValue } = useSettings()
  const { theme, setTheme } = useTheme()
  const [activeTab, setActiveTab] = usePersistentState<string>('settings_tab', 'general')
  const [ocrEngines, setOcrEngines] = useState<OcrEngineStatus[]>([])
  const [ocrTesting, setOcrTesting] = useState(false)
  const [ocrResult, setOcrResult] = useState<OcrTestResult | null>(null)
  const [deps, setDeps] = useState<DependencyStatus[]>([])
  const [appConfig, setAppConfig] = useState<ConfigInfo | null>(null)
  const [migrating, setMigrating] = useState(false)
  const [migrationStage, setMigrationStage] = useState<string | null>(null)
  const [migrationProgress, setMigrationProgress] = useState(0)
  const [localError, setLocalError] = useState<string | null>(null)
  const [version, setVersion] = useState<{ hash: string; time: string } | null>(null)
  const [funasrInstalling, setFunasrInstalling] = useState(false)
  const [bgeStatus, setBgeStatus] = useState<BgeStatus[] | null>(null)
  const [bgeInstalling, setBgeInstalling] = useState(false)
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const [caps, setCaps] = useState<AiCapabilities | null>(null)
  const [aiWarn, setAiWarn] = useState<string | null>(null)
  const [editingId, setEditingId] = useState<string | null>(null)
  const [editDraft, setEditDraft] = useState<{ name: string; baseUrl: string; apiKey: string; keyTouched: boolean; reveal: boolean } | null>(null)
  const [savingId, setSavingId] = useState<string | null>(null)
  const [testingId, setTestingId] = useState<string | null>(null)
  const [testOutcome, setTestOutcome] = useState<{ id: string; ok: boolean; detail: string } | null>(null)
  const [refreshingId, setRefreshingId] = useState<string | null>(null)
  const [refreshMsg, setRefreshMsg] = useState<{ id: string; text: string; isError: boolean } | null>(null)
  const [adding, setAdding] = useState(false)
  const [newProv, setNewProv] = useState({ name: '', baseUrl: '', apiKey: '' })
  const [modelFilter, setModelFilter] = useState<Record<string, string>>({})
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set())
  const [backupStatus, setBackupStatus] = useState<BackupStatus | null>(null)
  const [backingUp, setBackingUp] = useState(false)
  const [exporting, setExporting] = useState(false)
  const [exportPassword, setExportPassword] = useState('')
  const [deadDirs, setDeadDirs] = useState<DeadDir[]>([])
  const [backups, setBackups] = useState<BackupSnapshot[]>([])

  useEffect(() => {
    getVersion().then(setVersion).catch(() => {})
  }, [])

  useEffect(() => {
    listOcrEngines().then(setOcrEngines).catch(() => {})
  }, [])

  useEffect(() => {
    checkDependencies().then(setDeps).catch(() => {})
  }, [])

  useEffect(() => {
    getConfig().then(setAppConfig).catch(() => {})
  }, [])

  useEffect(() => {
    const unlisteners: (() => void)[] = []
    listen<MigrationProgress>('migration-progress', e => {
      setMigrationProgress(e.payload.progress)
      setMigrationStage(e.payload.stage)
    }).then(u => unlisteners.push(u))
    listen<MigrationWarning>('migration-warning', e => {
      void message(e.payload.message, { title: '迁移警告', kind: 'warning' })
    }).then(u => unlisteners.push(u))
    listen<FunasrInstallResult>('funasr-install-done', async e => {
      setFunasrInstalling(false)
      await message(e.payload.message, { title: 'FunASR', kind: e.payload.success ? 'info' : 'warning' })
      checkDependencies().then(setDeps).catch(() => {})
    }).then(u => unlisteners.push(u))
    listen<{ success: boolean; message: string }>('bge-install-done', async e => {
      setBgeInstalling(false)
      checkBgeInstalled().then(setBgeStatus).catch(() => {})
      if (e.payload.message) {
        await message(e.payload.message, { title: 'BGE', kind: e.payload.success ? 'info' : 'warning' })
      }
    }).then(u => unlisteners.push(u))
    return () => {
      unlisteners.forEach(u => u())
    }
  }, [])

  useEffect(() => {
    aiCapabilities().then(setCaps).catch(() => {})
  }, [])

  useEffect(() => {
    checkBgeInstalled().then(setBgeStatus).catch(() => {})
  }, [])

  useEffect(() => {
    getBackupStatus().then(setBackupStatus).catch(() => {})
    listBackups().then(setBackups).catch(() => {})
    getDeadDirs().then(setDeadDirs).catch(() => {})
    const id = setInterval(() => { getBackupStatus().then(setBackupStatus).catch(() => {}); listBackups().then(setBackups).catch(() => {}) }, 60_000)
    return () => clearInterval(id)
  }, [])

  const persistProviders = async (providers: ProviderInfo[]) => {
    if (!appConfig) return
    const updated = { ...appConfig, providers }
    await updateConfig(updated)
    setAppConfig(updated)
  }

  // 保存语义检索权重（P1 全局设置）
  const handleSaveSemanticWeight = async () => {
    if (!appConfig) return
    try {
      await updateConfig(appConfig)
    } catch (e) {
      setAiWarn(e instanceof Error ? e.message : String(e))
    }
  }

  const setProvidersLocal = (updater: (ps: ProviderInfo[]) => ProviderInfo[]) => {
    setAppConfig(c => (c ? { ...c, providers: updater(c.providers) } : c))
  }

  const providerInUse = (p: ProviderInfo) =>
    (appConfig?.active_embedding_model_id ?? '').startsWith(`${p.id}:`) ||
    (appConfig?.active_llm_model_id ?? '').startsWith(`${p.id}:`)

  const modelOptions = (kind: 'embedding' | 'llm'): { value: string; label: string }[] => {
    const remote = (appConfig?.providers ?? []).flatMap(p =>
      p.models
        .filter(m => m.enabled !== false && m.model_type === (kind === 'embedding' ? 'Embedding' : 'Llm'))
        .map(m => ({ value: `${p.id}:${m.id}`, label: `${p.name} / ${m.id}` })),
    )
    if (kind === 'embedding' && bgeStatus) {
      const localOpts = bgeStatus
        .filter(s => s.installed)
        .map(s => ({ value: `local:${s.model_dir.split('/').pop()}`, label: s.model_name }))
      return [...localOpts, ...remote]
    }
    return remote
  }

  const modelInUse = (p: ProviderInfo, modelId: string) =>
    appConfig?.active_embedding_model_id === `${p.id}:${modelId}` ||
    appConfig?.active_llm_model_id === `${p.id}:${modelId}`

  const handleToggleEnabled = async (p: ProviderInfo, modelId: string, enabled: boolean) => {
    await persistProviders(
      (appConfig?.providers ?? []).map(x =>
        x.id === p.id
          ? { ...x, models: x.models.map(m => (m.id === modelId ? { ...m, enabled } : m)) }
          : x,
      ),
    )
  }

  const handleActiveModel = async (kind: 'embedding' | 'llm', modelId: string) => {
    if (!appConfig) return
    const key = kind === 'embedding' ? 'active_embedding_model_id' : 'active_llm_model_id'
    const prev = appConfig[key]
    setAppConfig({ ...appConfig, [key]: modelId })
    // 重置该用途的可用状态为"检查中"，等待按新模型重新探测——避免
    // 沿用旧模型的可用性显示。
    setCaps(c => (c ? { ...c, [kind]: undefined } : c))
    try {
      await setActiveModel(kind, modelId)
      aiCapabilities().then(setCaps).catch(() => {})
    } catch (e) {
      setAppConfig(c => (c ? { ...c, [key]: prev } : c))
      setAiWarn(t('ai_error', { error: e instanceof Error ? e.message : String(e) }))
    }
  }

  const handleTestProvider = async (p: ProviderInfo) => {
    setTestingId(p.id)
    setTestOutcome(null)
    try {
      const r = await testProvider(p.base_url, p.api_key)
      setTestOutcome({ id: p.id, ok: r.ok, detail: r.detail })
    } catch (e) {
      setTestOutcome({ id: p.id, ok: false, detail: e instanceof Error ? e.message : String(e) })
    } finally {
      setTestingId(null)
    }
  }

  const handleRefreshProvider = async (p: ProviderInfo) => {
    setRefreshingId(p.id)
    setRefreshMsg(null)
    try {
      const models = await refreshProviderModels(p.id)
      setProvidersLocal(ps => ps.map(x => (x.id === p.id ? { ...x, models } : x)))
      setRefreshMsg({ id: p.id, text: t('ai_refresh_done', { n: models.length }), isError: false })
    } catch (e) {
      setRefreshMsg({ id: p.id, text: e instanceof Error ? e.message : String(e), isError: true })
    } finally {
      setRefreshingId(null)
    }
  }

  const handleDeleteProvider = async (p: ProviderInfo) => {
    const confirmed = await ask(t('confirm_delete_provider', { name: p.name }), { title: t('delete'), kind: 'warning' })
    if (!confirmed) return
    try {
      await deleteProvider(p.id)
      setProvidersLocal(ps => ps.filter(x => x.id !== p.id))
    } catch (e) {
      setAiWarn(e instanceof Error ? e.message : String(e))
    }
  }

  const openEdit = (p: ProviderInfo) => {
    setEditingId(p.id)
    setEditDraft({ name: p.name, baseUrl: p.base_url, apiKey: maskApiKey(p.api_key), keyTouched: false, reveal: false })
  }

  const handleSaveEdit = async (p: ProviderInfo) => {
    if (!editDraft) return
    const mask = maskApiKey(p.api_key)
    const finalKey = editDraft.keyTouched && editDraft.apiKey !== '' && editDraft.apiKey !== mask ? editDraft.apiKey : p.api_key
    setSavingId(p.id)
    try {
      await persistProviders(
        (appConfig?.providers ?? []).map(x =>
          x.id === p.id
            ? { ...x, name: editDraft.name, base_url: editDraft.baseUrl, api_key: finalKey }
            : x,
        ),
      )
      setEditingId(null)
      setEditDraft(null)
    } catch (e) {
      setAiWarn(t('ai_error', { error: e instanceof Error ? e.message : String(e) }))
    } finally {
      setSavingId(null)
    }
  }

  const handleModelType = async (p: ProviderInfo, modelId: string, modelType: ModelType) => {
    try {
      await persistProviders(
        (appConfig?.providers ?? []).map(x =>
          x.id === p.id
            ? { ...x, models: x.models.map(m => (m.id === modelId ? { ...m, model_type: modelType } : m)) }
            : x,
        ),
      )
    } catch (e) {
      setAiWarn(t('ai_error', { error: e instanceof Error ? e.message : String(e) }))
    }
  }

  const handleAddProvider = async () => {
    if (!newProv.name.trim() || !newProv.baseUrl.trim()) {
      setAiWarn(t('ai_add_required'))
      return
    }
    try {
      const out = await addProvider(newProv.name.trim(), newProv.baseUrl.trim(), newProv.apiKey)
      if (out.pull_error) setAiWarn(t('ai_pull_error', { detail: out.pull_error }))
      setProvidersLocal(ps => [
        ...ps,
        { id: out.id, name: newProv.name.trim(), base_url: newProv.baseUrl.trim(), api_key: newProv.apiKey, models: [] },
      ])
      const fresh = await getConfig()
      setAppConfig(fresh)
      setAdding(false)
      setNewProv({ name: '', baseUrl: '', apiKey: '' })
    } catch (e) {
      setAiWarn(t('ai_error', { error: e instanceof Error ? e.message : String(e) }))
    }
  }

  const [aiTest, setAiTest] = useState<{ kind: string; ok: boolean; detail: string }[] | null>(null)
  const [aiTestLoading, setAiTestLoading] = useState(false)
  const testAi = useCallback(async () => {
    setAiTestLoading(true)
    setAiTest(null)
    try {
      const results = await invoke<{ kind: string; ok: boolean; detail: string }[]>('test_ai_gateway')
      setAiTest(results)
    } catch (e) {
      setAiTest([{ kind: 'error', ok: false, detail: e instanceof Error ? e.message : '测试失败' }])
    } finally {
      setAiTestLoading(false)
    }
  }, [])

  const handleRestoreFromBackup = async (backupName: string) => {
    const confirmed = await ask(t('confirm_restore_backup', { name: backupName }), { title: t('backup_restore'), kind: 'warning' })
    if (!confirmed) return
    try {
      await restoreBackup(backupName)
      await message(t('backup_restore_completed'), { title: t('backup_restore'), kind: 'info' })
    } catch (e) {
      setLocalError(e instanceof Error ? e.message : String(e))
    }
  }

  const handleDeleteBackup = async (backupName: string) => {
    const confirmed = await ask(t('confirm_delete_backup', { name: backupName }), { title: t('backup_delete'), kind: 'warning' })
    if (!confirmed) return
    try {
      await deleteBackup(backupName)
      listBackups().then(setBackups).catch(() => {})
      getBackupStatus().then(setBackupStatus).catch(() => {})
    } catch (e) {
      setLocalError(e instanceof Error ? e.message : String(e))
    }
  }

  const handleFunasrInstall = async () => {
    if (funasrInstalling) return
    const confirmed = await ask(t('confirm_install_funasr'), {
      title: t('funasr_install_prompt'),
      kind: 'warning',
      okLabel: t('install_now'),
      cancelLabel: t('not_now'),
    })
    if (!confirmed) return
    setFunasrInstalling(true)
    try {
      await installFunasr()
    } catch (e) {
      setFunasrInstalling(false)
      setLocalError(e instanceof Error ? e.message : String(e))
    }
  }

  const handleTestOcr = async () => {
    const engineType = settings['ocr_engine'] ?? 'PaddleOCR'
    setOcrTesting(true)
    setOcrResult(null)
    try {
      const result = await testOcrEngine(engineType)
      setOcrResult(result)
    } catch (e) {
      setOcrResult({ success: false, text: '', duration_ms: 0, error: String(e) })
    } finally {
      setOcrTesting(false)
    }
  }

  const handleChangeDataDir = async () => {
    const selected = await open({ directory: true, multiple: false, title: t('data_directory') })
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
      const restart = await ask(msg, { title: '迁移完成', kind: 'info' })
      if (restart) await restartApp()
    } catch (e: unknown) {
      const err = e instanceof Error ? e.message : String(e)
      setLocalError(`迁移失败: ${err}`)
      await message(`迁移失败:\n${err}`, { title: '迁移失败', kind: 'error' })
    } finally {
      setMigrating(false)
      setMigrationStage(null)
    }
  }

  const handleChangeLang = async (newLang: string) => {
    await setLang(newLang as 'zh' | 'en' | 'ja' | 'ko')
  }

  const handleBackupNow = async () => {
    setBackingUp(true)
    try {
      await triggerBackup()
      const s = await getBackupStatus()
      setBackupStatus(s)
    } catch (e) {
      setLocalError(e instanceof Error ? e.message : String(e))
    } finally {
      setBackingUp(false)
    }
  }

  const handleExportBackup = async (backupName?: string) => {
    if (exporting) return
    try {
      setExporting(true)
      const dest = await save({
        filters: [{ name: 'ZIP', extensions: ['zip'] }],
        defaultPath: `link-searcher-backup-${backupName ?? new Date().toISOString().slice(0, 10)}.zip`,
        title: t('backup_export_zip'),
      })
      if (!dest) return
      const result = await exportBackup(dest as string, exportPassword || undefined, backupName || null)
      if (result.has_secrets) {
        await message(t('backup_export_no_password_warning'), { title: t('backup_export_current'), kind: 'warning' })
      }
      await message(t('backup_export_done', { path: result.dest_path }), { title: t('backup_export_current'), kind: 'info' })
      listBackups().then(setBackups).catch(() => {})
    } catch (e) {
      setLocalError(e instanceof Error ? e.message : String(e))
    } finally {
      setExporting(false)
    }
  }

  const handleRestoreZip = async () => {
    try {
      const file = await open({ directory: false, multiple: false, title: t('backup_restore_select'), filters: [{ name: 'ZIP', extensions: ['zip'] }] })
      if (!file) return
      const confirmed = await ask(t('confirm_rebuild'), { title: t('backup_restore'), kind: 'warning' })
      if (!confirmed) return
      await restoreFromZip(file as string, exportPassword || undefined)
      const restart = await ask(t('backup_restore'), { title: t('backup_restore'), kind: 'info' })
      if (restart) await restartApp()
    } catch (e) {
      setLocalError(e instanceof Error ? e.message : String(e))
    }
  }

  const handleRemapDir = async (dirId: string) => {
    try {
      const newPath = await open({ directory: true, multiple: false, title: t('backup_remap_select') })
      if (!newPath) return
      await remapDir(dirId, newPath as string)
      setDeadDirs(d => d.filter(x => x.id !== dirId))
    } catch (e) {
      setLocalError(e instanceof Error ? e.message : String(e))
    }
  }

  const handleRemoveDir = async (dirId: string) => {
    const confirmed = await ask(t('confirm_remove_dir'), { title: t('backup_remove'), kind: 'warning' })
    if (!confirmed) return
    try {
      await removeDirWithFiles(dirId)
      setDeadDirs(d => d.filter(x => x.id !== dirId))
    } catch (e) {
      setLocalError(e instanceof Error ? e.message : String(e))
    }
  }

  const selectedEngine = ocrEngines.find(e => e.engine_type === (settings['ocr_engine'] ?? 'PaddleOCR'))

  const handleFieldChange = (key: string, value: string) => {
    setValue(key, value)
    setLocalError(null)
    if (saveTimerRef.current) clearTimeout(saveTimerRef.current)
    saveTimerRef.current = setTimeout(() => {
      updateSettings({ [key]: value })
        .catch(e => setLocalError(e instanceof Error ? e.message : 'Failed to save setting'))
    }, 300)
  }

  useEffect(() => {
    return () => {
      if (saveTimerRef.current) clearTimeout(saveTimerRef.current)
    }
  }, [])

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
        <div className={activeTab === 'general' ? 'space-y-6' : 'hidden'}>
        <Section title={t('data_directory')}>
          <div className="text-sm text-gray-700 dark:text-gray-300 font-mono break-all">
            {appConfig?.data_dir || t('loading')}
          </div>
          <button
            onClick={handleChangeDataDir}
            disabled={migrating}
            className="mt-2 flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded-md hover:bg-blue-100 dark:hover:bg-blue-900/50 disabled:opacity-50 transition-colors"
          >
            {migrating && <LoadingSpinner className="size-3" />}
            {t('migrate_data')}
          </button>
          {migrating && (
            <div className="mt-2">
              <div className="w-full h-1.5 bg-gray-200 dark:bg-gray-700 rounded-full overflow-hidden">
                <div
                  className="h-full bg-blue-500 transition-all"
                  style={{ width: `${migrationProgress}%` }}
                />
              </div>
              <p className="text-xs text-gray-500 dark:text-gray-400 mt-1">{migrationStage}</p>
            </div>
          )}
        </Section>
        </div>

        <div className={activeTab === 'docs' ? 'space-y-6' : 'hidden'}>
        <Section title={t('doc_engine')}>
          <p className="text-sm text-gray-600 dark:text-gray-400">
            <span className="font-semibold text-gray-900 dark:text-gray-100">Native</span>
            {' '}— {t('doc_engine_desc')}
          </p>
        </Section>
        </div>

        <div className={activeTab === 'index' ? 'space-y-6' : 'hidden'}>
        <Section title={t('ocr_engine')}>
          <p className="text-sm text-gray-600 dark:text-gray-400 mb-2">
            当前: <span className="font-semibold text-gray-900 dark:text-gray-100">{selectedEngine?.name ?? '未选择'}</span>
          </p>
          <div className="space-y-2">
            {ocrEngines.map(engine => (
              <label key={engine.engine_type} className="flex items-center gap-3 p-2 rounded-lg hover:bg-gray-50 dark:hover:bg-gray-800 cursor-pointer">
                <input
                  type="radio"
                  name="ocr_engine"
                  value={engine.engine_type}
                  checked={settings['ocr_engine'] === engine.engine_type}
                  onChange={() => handleFieldChange('ocr_engine', engine.engine_type)}
                  className="text-blue-600"
                />
                <div className="flex-1">
                  <span className="text-sm text-gray-900 dark:text-gray-100">{engine.name}</span>
                  {!engine.available && (
                    <span className="ml-2 text-xs text-amber-600 dark:text-amber-400">({t('not_installed')})</span>
                  )}
                </div>
              </label>
            ))}
          </div>

          <button
            onClick={handleTestOcr}
            disabled={ocrTesting}
            className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded-lg hover:bg-blue-100 dark:hover:bg-blue-900/50 disabled:opacity-50 transition-colors"
          >
            {ocrTesting && <LoadingSpinner className="size-4" />}
            {t('test_ocr')}
          </button>

          {ocrResult && (
            <div className={`p-3 rounded-lg text-sm ${
              ocrResult.success
                ? 'bg-green-50 dark:bg-green-900/20 text-green-700 dark:text-green-400'
                : 'bg-red-50 dark:bg-red-900/20 text-red-700 dark:text-red-400'
            }`}>
              {ocrResult.success
                ? `✅ ${t('ocr_test_success')}: "${ocrResult.text}" (${ocrResult.duration_ms}ms)`
                : `❌ ${t('ocr_test_failed')}: ${ocrResult.error ?? 'Unknown'}`
              }
            </div>
          )}

          {selectedEngine && !selectedEngine.available && (
            <div className="p-3 bg-amber-50 dark:bg-amber-900/20 rounded-lg text-sm text-amber-700 dark:text-amber-400">
              <p className="font-medium mb-2">Install Guide:</p>
              <pre className="whitespace-pre-wrap text-xs font-mono">{selectedEngine.install_guide}</pre>
            </div>
          )}

          <p className="text-xs text-gray-500 dark:text-gray-400">
            ⚠️ {t('ocr_engine_required')}
          </p>
        </Section>

        <Section title={t('ocr_lang_section')}>
          <SelectField
            label={t('ocr_lang')}
            value={settings['ocr_lang'] ?? 'eng'}
            onChange={v => handleFieldChange('ocr_lang', v)}
            options={OCR_LANGS}
          />
        </Section>
        </div>

        <div className={activeTab === 'general' ? 'space-y-6' : 'hidden'}>
        <Section title={t('language')}>
          <SelectField
            label={t('language')}
            value={lang}
            onChange={handleChangeLang}
            options={LANG_OPTIONS.map(o => ({ value: o.value, label: t(o.labelKey) }))}
          />
        </Section>
        </div>

        <div className={activeTab === 'ai' ? 'space-y-6' : 'hidden'}>
        <Section title={t('ai_service')}>
          <p className="text-xs text-gray-500 dark:text-gray-400 mb-3">{t('ai_service_desc')}</p>
          {aiWarn && (
            <div className="px-3 py-2 text-xs text-amber-700 dark:text-amber-400 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-900 rounded-lg">
              {aiWarn}
            </div>
          )}

          <div className="text-xs font-semibold text-gray-700 dark:text-gray-300">{t('ai_current_usage')}</div>
          {/* P1 检索策略：语义 vs 关键词权重滑杆 */}
          <div className="p-3 bg-gray-50 dark:bg-gray-800/40 border border-gray-200 dark:border-gray-700 rounded-lg">
            <label className="block text-xs font-medium text-gray-600 dark:text-gray-300 mb-1">
              {t('retrieval_strategy')} — {t('semantic_weight_label')}
            </label>
            <input
              type="range"
              min="0" max="1" step="0.05"
              value={appConfig?.semantic_weight ?? 0.3}
              onChange={e => { const v = Number(e.target.value); setAppConfig(c => c ? { ...c, semantic_weight: v } : c) }}
              className="w-full accent-purple-600"
            />
            <div className="flex justify-between text-[10px] text-gray-400 mt-0.5">
              <span>{t('keyword_label')}</span>
              <span>{(appConfig?.semantic_weight ?? 0.3) >= 0.5 ? t('semantic_label') : (appConfig?.semantic_weight ?? 0.3)}</span>
              <span>{t('semantic_label')}</span>
            </div>
            <p className="text-[10px] text-gray-400 mt-1">{t('semantic_weight_hint')}</p>
            <button
              type="button"
              onClick={handleSaveSemanticWeight}
              className="mt-2 px-2 py-0.5 text-[10px] font-medium text-white bg-purple-600 hover:bg-purple-700 rounded transition-colors"
            >
              {t('save')}
            </button>
          </div>
          <div className="space-y-3">
            <UsageSelect
              label={t('embedding_model')}
              value={appConfig?.active_embedding_model_id ?? ''}
              onChange={v => handleActiveModel('embedding', v)}
              options={modelOptions('embedding')}
              cap={caps?.embedding}
              notSelectedLabel={t('ai_not_selected')}
              checkingLabel={t('ai_checking')}
              availableLabel={t('ai_available')}
              notConfiguredLabel={t('ai_not_configured')}
            />
            {bgeStatus && !bgeStatus.some(s => s.installed) && (
              <button
                onClick={() => {
                  setBgeInstalling(true)
                  installBge().catch(e => {
                    setBgeInstalling(false)
                    setLocalError(e instanceof Error ? e.message : String(e))
                  })
                }}
                disabled={bgeInstalling}
                className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-emerald-600 dark:text-emerald-400 bg-emerald-50 dark:bg-emerald-900/30 rounded-lg hover:bg-emerald-100 dark:hover:bg-emerald-900/50 disabled:opacity-50 transition-colors"
              >
                {bgeInstalling && <LoadingSpinner className="size-3" />}
                {bgeInstalling ? t('bge_downloading') : t('bge_download')}
              </button>
            )}
            <UsageSelect
              label={t('llm_model')}
              value={appConfig?.active_llm_model_id ?? ''}
              onChange={v => handleActiveModel('llm', v)}
              options={modelOptions('llm')}
              cap={caps?.llm}
              notSelectedLabel={t('ai_not_selected')}
              checkingLabel={t('ai_checking')}
              availableLabel={t('ai_available')}
              notConfiguredLabel={t('ai_not_configured')}
            />
          </div>

          <div className="pt-2 space-y-1 flex items-center gap-2 flex-wrap">
            <button
              onClick={testAi}
              disabled={aiTestLoading}
              className="px-3 py-1.5 text-xs font-medium text-white bg-purple-600 hover:bg-purple-700 rounded disabled:opacity-50 transition-colors"
            >
              {aiTestLoading ? '…' : t('test_ai_gateways')}
            </button>
            {aiTest && (
              <div className="flex items-center gap-3">
                {aiTest.map((r, i) => (
                  <div key={i} className={`flex items-center gap-1 text-xs ${r.ok ? 'text-green-600 dark:text-green-400' : 'text-red-600 dark:text-red-400'}`}>
                    <span>{r.ok ? '✓' : '✗'}</span>
                    <span className="font-medium">{r.kind === 'embedding' ? t('embedding_gateway') : t('llm_gateway')}</span>
                    <span className="text-gray-500 dark:text-gray-400 truncate max-w-48">{r.detail}</span>
                  </div>
                ))}
              </div>
            )}
          </div>

          <div>
            <div className="text-xs font-semibold text-gray-700 dark:text-gray-300 flex items-center gap-2 mb-2">
              <span className="size-1.5 rounded-full bg-purple-500" />
              {t('ai_providers')}
            </div>
            {(appConfig?.providers ?? []).length === 0 ? (
              <p className="text-xs text-gray-500 dark:text-gray-400">{t('ai_no_provider')}</p>
            ) : (
              <div className="space-y-2">
                {(appConfig?.providers ?? []).map(p => (
                  <div key={p.id} className="p-3 bg-gray-50 dark:bg-gray-800/40 border border-gray-200 dark:border-gray-700 rounded-lg">
                    <div className="flex items-center gap-2">
                      <div className="flex-1 min-w-0">
                        <p className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">{p.name}</p>
                        <p className="text-xs text-gray-500 dark:text-gray-400 font-mono truncate">{p.base_url}</p>
                      </div>
                      <span className="text-xs text-gray-500 dark:text-gray-400 shrink-0">{t('ai_models_count', { n: p.models.length })}</span>
                    </div>
                    <div className="flex items-center gap-1.5 mt-2">
                      <RowAction onClick={() => handleTestProvider(p)} disabled={testingId === p.id}>
                        {testingId === p.id ? '…' : t('ai_test')}
                      </RowAction>
                      <RowAction onClick={() => openEdit(p)}>{t('ai_edit')}</RowAction>
                      <RowAction onClick={() => handleRefreshProvider(p)} disabled={refreshingId === p.id}>
                        {refreshingId === p.id ? '…' : t('refresh')}
                      </RowAction>
                      <RowAction danger onClick={() => handleDeleteProvider(p)} disabled={providerInUse(p)} title={providerInUse(p) ? t('ai_delete_in_use') : undefined}>
                        {t('delete')}
                      </RowAction>
                    </div>
                    {testOutcome?.id === p.id && (
                      <div className={`mt-2 flex items-center gap-1 text-xs ${testOutcome.ok ? 'text-green-600 dark:text-green-400' : 'text-red-600 dark:text-red-400'}`}>
                        <span>{testOutcome.ok ? '✓' : '✗'}</span>
                        <span className="font-medium shrink-0">{testOutcome.ok ? t('ai_test_ok') : t('ai_test_fail')}</span>
                        <span className="text-gray-500 dark:text-gray-400 truncate">{testOutcome.detail}</span>
                      </div>
                    )}
                    {refreshMsg?.id === p.id && (
                      <div className={`mt-2 text-xs ${refreshMsg.isError ? 'text-red-600 dark:text-red-400' : 'text-green-600 dark:text-green-400'}`}>
                        {refreshMsg.text}
                      </div>
                    )}
                    {p.models.length > 0 && (
                      <div className="mt-2 pt-2 border-t border-gray-200 dark:border-gray-700">
                        {(() => {
                          const enabledModels = p.models.filter(m => m.enabled)
                          const filter = (modelFilter[p.id] ?? '').toLowerCase()
                          const expanded = (key: string) => filter !== '' || expandedGroups.has(key)
                          return (
                            <>
                              {/* 已启用区：直接显示，供快速改类型/停用 */}
                              {enabledModels.length > 0 && (
                                <div className="mb-2">
                                  <div className="text-xs font-medium text-gray-600 dark:text-gray-300 mb-1">
                                    {t('ai_enabled_models', { n: enabledModels.length })}
                                  </div>
                                  {(['Embedding', 'Llm', 'Unknown'] as const).map(group => {
                                    const matched = enabledModels.filter(m => m.model_type === group)
                                    if (matched.length === 0) return null
                                    const labelKey = group === 'Embedding' ? 'model_group_embedding' : group === 'Llm' ? 'model_group_llm' : 'model_group_unknown'
                                    return (
                                      <div key={`en-${group}`} className="mb-1">
                                        <div className="text-[10px] text-gray-400 dark:text-gray-500 px-1">{t(labelKey, { n: matched.length })}</div>
                                        {matched.map(m => (
                                          <div key={m.id} className="flex items-center gap-2 pl-1">
                                            <span className="flex-1 text-xs font-mono text-gray-700 dark:text-gray-300 truncate px-0.5">{m.id}</span>
                                            <select
                                              title={t('ai_model_type')}
                                              value={m.model_type}
                                              onChange={e => handleModelType(p, m.id, e.target.value as ModelType)}
                                              className="px-1.5 py-0.5 text-xs bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-blue-500 transition-colors"
                                            >
                                              <option value="Embedding">{t('ai_type_embedding')}</option>
                                              <option value="Llm">{t('ai_type_llm')}</option>
                                              <option value="Unknown">{t('ai_type_unknown')}</option>
                                            </select>
                                            <button
                                              type="button"
                                              onClick={() => handleToggleEnabled(p, m.id, false)}
                                              disabled={modelInUse(p, m.id)}
                                              title={modelInUse(p, m.id) ? t('ai_model_in_use') : t('ai_disable')}
                                              className="px-1 text-xs text-red-500 hover:text-red-600 disabled:opacity-30 disabled:cursor-not-allowed shrink-0"
                                            >
                                              ×
                                            </button>
                                          </div>
                                        ))}
                                      </div>
                                    )
                                  })}
                                </div>
                              )}
                              {/* 全量搜索框 */}
                              <input
                                type="text"
                                value={modelFilter[p.id] ?? ''}
                                onChange={e => setModelFilter(f => ({ ...f, [p.id]: e.target.value }))}
                                placeholder={t('model_filter_placeholder')}
                                className="mb-1.5 w-full px-2 py-1 text-xs bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500 transition-colors"
                              />
                              {/* 折叠的全量列表 */}
                              <div className="space-y-1">
                                {(['Embedding', 'Llm', 'Unknown'] as const).map(group => {
                                  const matched = p.models.filter(m =>
                                    m.model_type === group && (!filter || m.id.toLowerCase().includes(filter)))
                                  if (matched.length === 0) return null
                                  const key = `${p.id}:${group}`
                                  const labelKey = group === 'Embedding' ? 'model_group_embedding' : group === 'Llm' ? 'model_group_llm' : 'model_group_unknown'
                                  const isExpanded = expanded(key)
                                  return (
                                    <div key={group}>
                                      <button
                                        type="button"
                                        onClick={() => setExpandedGroups(s => {
                                          const n = new Set(s)
                                          if (n.has(key)) n.delete(key)
                                          else n.add(key)
                                          return n
                                        })}
                                        className="w-full flex items-center gap-1.5 px-1 py-0.5 text-xs font-medium text-gray-600 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-200 transition-colors"
                                      >
                                        <span className="text-[10px]">{isExpanded ? '▾' : '▸'}</span>
                                        <span className="flex-1 text-left">{t(labelKey, { n: matched.length })}</span>
                                      </button>
                                      {isExpanded && matched.map(m => (
                                        <div key={m.id} className="flex items-center gap-2 pl-3">
                                          <span
                                            className={`px-1 rounded text-[10px] shrink-0 ${
                                              m.model_type === 'Embedding'
                                                ? 'bg-purple-100 text-purple-700 dark:bg-purple-900/40 dark:text-purple-300'
                                                : m.model_type === 'Llm'
                                                  ? 'bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-300'
                                                  : 'bg-gray-100 text-gray-500 dark:bg-gray-800 dark:text-gray-400'
                                            }`}
                                          >
                                            {m.model_type === 'Embedding' ? 'Embed' : m.model_type === 'Llm' ? 'LLM' : '?'}
                                          </span>
                                          <span className="flex-1 text-xs font-mono text-gray-700 dark:text-gray-300 truncate px-0.5">{m.id}</span>
                                          {m.enabled ? (
                                            <span className="text-[10px] text-green-600 dark:text-green-400 shrink-0">{t('ai_enabled_tag')}</span>
                                          ) : (
                                            <button
                                              type="button"
                                              onClick={() => handleToggleEnabled(p, m.id, true)}
                                              className="px-1 text-[10px] text-purple-600 hover:text-purple-700 dark:text-purple-400 dark:hover:text-purple-300 shrink-0"
                                            >
                                              ＋ {t('ai_enable')}
                                            </button>
                                          )}
                                        </div>
                                      ))}
                                    </div>
                                  )
                                })}
                              </div>
                            </>
                          )
                        })()}
                      </div>
                    )}
                    {editingId === p.id && editDraft && (
                      <div className="mt-3 space-y-3 p-3 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-700 rounded-lg">
                        <div>
                          <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{t('ai_name')}</label>
                          <input
                            type="text"
                            value={editDraft.name}
                            onChange={e => setEditDraft({ ...editDraft, name: e.target.value })}
                            className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
                          />
                        </div>
                        <div>
                          <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{t('ai_base_url')}</label>
                          <input
                            type="text"
                            value={editDraft.baseUrl}
                            onChange={e => setEditDraft({ ...editDraft, baseUrl: e.target.value })}
                            className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
                          />
                        </div>
                        <div>
                          <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{t('ai_api_key')}</label>
                          <div className="flex gap-1.5">
                            <input
                              type={editDraft.reveal ? 'text' : 'password'}
                              value={editDraft.reveal && !editDraft.keyTouched ? p.api_key : editDraft.apiKey}
                              onChange={e => setEditDraft({ ...editDraft, apiKey: e.target.value, keyTouched: true })}
                              placeholder={t('ai_key_placeholder')}
                              className="flex-1 px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
                            />
                            <button
                              type="button"
                              onClick={() => setEditDraft(d => {
                                if (!d) return d
                                const reveal = !d.reveal
                                return { ...d, reveal, apiKey: d.keyTouched ? d.apiKey : (reveal ? p.api_key : maskApiKey(p.api_key)) }
                              })}
                              title={editDraft.reveal ? t('ai_hide_key') : t('ai_show_key')}
                              className="px-2.5 text-sm text-gray-500 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 border border-gray-200 dark:border-gray-700 rounded-lg bg-gray-50 dark:bg-gray-800 transition-colors"
                            >
                              👁
                            </button>
                          </div>
                        </div>
                        <div className="flex gap-2">
                          <button
                            onClick={() => handleSaveEdit(p)}
                            disabled={savingId === p.id}
                            className="px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 rounded disabled:opacity-50 transition-colors"
                          >
                            {savingId === p.id ? '…' : t('ai_save')}
                          </button>
                          <button
                            onClick={() => { setEditingId(null); setEditDraft(null) }}
                            className="px-3 py-1.5 text-xs font-medium text-gray-600 dark:text-gray-300 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 rounded transition-colors"
                          >
                            {t('cancel')}
                          </button>
                        </div>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>

          {!adding ? (
            <button
              onClick={() => setAdding(true)}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded-lg hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
            >
              <PlusIcon className="size-3.5" />
              {t('ai_add_provider')}
            </button>
          ) : (
            <div className="space-y-3 p-3 bg-gray-50 dark:bg-gray-800/40 border border-gray-200 dark:border-gray-700 rounded-lg">
              <div>
                <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{t('ai_name')}</label>
                <input
                  type="text"
                  value={newProv.name}
                  onChange={e => setNewProv({ ...newProv, name: e.target.value })}
                  className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
                />
              </div>
              <div>
                <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{t('ai_base_url')}</label>
                <input
                  type="text"
                  value={newProv.baseUrl}
                  onChange={e => setNewProv({ ...newProv, baseUrl: e.target.value })}
                  placeholder="http://127.0.0.1:11434/v1"
                  className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
                />
              </div>
              <div>
                <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{t('ai_api_key')}</label>
                <input
                  type="text"
                  value={newProv.apiKey}
                  onChange={e => setNewProv({ ...newProv, apiKey: e.target.value })}
                  placeholder={t('ai_key_placeholder')}
                  className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
                />
              </div>
              <div className="flex gap-2">
                <button
                  onClick={handleAddProvider}
                  className="px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 rounded transition-colors"
                >
                  {t('ai_save')}
                </button>
                <button
                  onClick={() => setAdding(false)}
                  className="px-3 py-1.5 text-xs font-medium text-gray-600 dark:text-gray-300 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 rounded transition-colors"
                >
                  {t('cancel')}
                </button>
              </div>
            </div>
          )}
        </Section>
        </div>

        <div className={activeTab === 'backup' ? 'space-y-6' : 'hidden'}>
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
              onClick={handleBackupNow}
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
              onChange={e => setExportPassword(e.target.value)}
              placeholder={t('backup_export_password_placeholder')}
              className="flex-1 px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
            />
            <button
              onClick={() => handleExportBackup()}
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
              onClick={handleRestoreZip}
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
                    onClick={() => handleRemapDir(d.id)}
                    className="px-2 py-1 text-xs font-medium text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30 rounded hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
                  >
                    {t('backup_remap')}
                  </button>
                  <button
                    onClick={() => handleRemoveDir(d.id)}
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
                  <span className={`shrink-0 w-2 h-2 rounded-full ${snap.kind === 'baseline' || snap.kind === 'merged' ? 'bg-green-500' : 'bg-blue-400'}`} />
                  <span className="font-mono text-gray-900 dark:text-gray-100 flex-1 truncate">{snap.id}</span>
                  <span className="text-gray-500 dark:text-gray-400 shrink-0">{new Date(snap.ts * 1000).toLocaleDateString()} {new Date(snap.ts * 1000).toLocaleTimeString()}</span>
                  <span className="text-gray-400 dark:text-gray-500 shrink-0">{formatSize(snap.size)}</span>
                  <button
                    onClick={() => handleExportBackup(snap.id)}
                    disabled={exporting}
                    title={t('backup_export_snapshot')}
                    className="px-2 py-0.5 text-xs rounded bg-gray-100 dark:bg-gray-700 text-gray-700 dark:text-gray-200 hover:bg-gray-200 dark:hover:bg-gray-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                  >
                    {t('backup_export_snapshot_short')}
                  </button>
                  <button
                    onClick={() => handleDeleteBackup(snap.id)}
                    title={t('backup_delete')}
                    className="px-2 py-0.5 text-xs rounded bg-red-50 dark:bg-red-900/20 text-red-600 dark:text-red-400 hover:bg-red-100 dark:hover:bg-red-900/40 transition-colors"
                  >
                    {t('backup_delete')}
                  </button>
                </div>
              ))}
            </div>
            <p className="mt-2 text-xs text-gray-400 dark:text-gray-500">{t('backup_keep_policy', { n: 10 })}</p>
          </Section>
        )}
        </div>

        <div className={activeTab === 'system' ? 'space-y-6' : 'hidden'}>
        <Section title={t('tab_system')}>
          <ToggleField
            label={t('sys_launch_on_startup')}
            checked={settings['auto_start'] === 'true'}
            onChange={v => handleFieldChange('auto_start', v ? 'true' : 'false')}
          />
        </Section>

        <Section title={t('sys_scheduling')}>
          <TextField
            label={t('sys_scheduled_scan_time')}
            value={settings['scan_time'] ?? '02:00'}
            onChange={v => handleFieldChange('scan_time', v)}
            placeholder="Default: 02:00 (2 AM)"
          />
          <ToggleField
            label={t('sys_auto_backup')}
            checked={settings['auto_backup'] === 'true'}
            onChange={v => handleFieldChange('auto_backup', v ? 'true' : 'false')}
          />
          <NumberField
            label={t('sys_backup_interval')}
            value={parseInt(settings['backup_interval'] ?? '7', 10)}
            onChange={v => handleFieldChange('backup_interval', String(v))}
            min={1}
            max={365}
            placeholder="Default: 7"
          />
          <NumberField
            label={t('sys_max_results')}
            value={parseInt(settings['max_results'] ?? '1000', 10)}
            onChange={v => handleFieldChange('max_results', String(v))}
            min={100}
            max={10000}
            step={100}
            placeholder="Default: 1000"
          />
        </Section>

        <Section title={t('sys_exclusions')}>
          <TextareaField
            label={t('sys_exclude_patterns')}
            value={settings['exclude_patterns'] ?? ''}
            onChange={v => handleFieldChange('exclude_patterns', v)}
            placeholder="*.tmp&#10;node_modules&#10;.git"
            rows={4}
          />
        </Section>
        </div>

        <div className={activeTab === 'index' ? 'space-y-6' : 'hidden'}>
        <Section title={t('dependencies')}>
          {deps.map(dep => (
            <div key={dep.command} className="flex items-start gap-3">
              {dep.available
                ? <span className="text-green-600 dark:text-green-400 text-lg">✓</span>
                : <span className="text-amber-600 dark:text-amber-400 text-lg">✗</span>
              }
              <div className="flex-1">
                <p className="text-sm text-gray-900 dark:text-gray-100">{dep.name}</p>
                <p className="text-xs text-gray-500">{dep.command}</p>
                {!dep.available && (
                  <pre className="mt-1 text-xs text-gray-600 dark:text-gray-400 whitespace-pre-wrap bg-gray-50 dark:bg-gray-800 p-2 rounded">{filterGuide(dep.install_guide)}</pre>
                )}
              </div>
              {!dep.available && dep.name.includes('FunASR') && (
                <button
                  onClick={handleFunasrInstall}
                  disabled={funasrInstalling}
                  className="shrink-0 px-3 py-1.5 text-xs font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed rounded-lg transition-colors"
                >
                  {funasrInstalling ? t('installing') : t('install_now')}
                </button>
              )}
            </div>
          ))}
        </Section>
        </div>

        <div className={activeTab === 'general' ? 'space-y-6' : 'hidden'}>
        <Section title={t('theme')}>
          <SelectField
            label={t('theme')}
            value={theme}
            onChange={v => setTheme(v as 'light' | 'dark' | 'system')}
            options={[
              { value: 'light', label: t('light') },
              { value: 'dark', label: t('dark') },
              { value: 'system', label: t('system') },
            ]}
          />
        </Section>
        </div>

        {version && (
          <div className="text-center text-xs text-gray-400 dark:text-gray-600 mt-4 pt-4 border-t border-gray-100 dark:border-gray-800">
            git: {version.hash} ({version.time})
          </div>
        )}
      </div>
    </div>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="p-4 bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 rounded-lg">
      <h3 className="text-sm font-medium text-gray-900 dark:text-gray-100 mb-4">{title}</h3>
      <div className="space-y-4">{children}</div>
    </div>
  )
}

function TextField({ label, value, onChange, placeholder, onBlur }: {
  label: string; value: string; onChange: (v: string) => void; placeholder?: string; onBlur?: () => void
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{label}</label>
      <input
        type="text"
        value={value}
        onChange={e => onChange(e.target.value)}
        onBlur={onBlur}
        placeholder={placeholder}
        className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
      />
    </div>
  )
}

function SelectField({ label, value, onChange, options }: {
  label: string; value: string; onChange: (v: string) => void; options: { value: string; label: string }[]
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{label}</label>
      <select
        value={value}
        onChange={e => onChange(e.target.value)}
        className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
      >
        {options.map(o => (
          <option key={o.value} value={o.value}>{o.label}</option>
        ))}
      </select>
    </div>
  )
}

function NumberField({ label, value, onChange, min, max, step, placeholder }: {
  label: string; value: number; onChange: (v: number) => void; min: number; max: number; step?: number; placeholder?: string
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{label}</label>
      <input
        type="number"
        value={value}
        onChange={e => {
          const v = parseInt(e.target.value, 10)
          onChange(Number.isNaN(v) ? min : Math.max(min, v))
        }}
        min={min}
        max={max}
        step={step}
        placeholder={placeholder}
        className="w-24 px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
      />
    </div>
  )
}

function TextareaField({ label, value, onChange, placeholder, rows }: {
  label: string; value: string; onChange: (v: string) => void; placeholder?: string; rows?: number
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{label}</label>
      <textarea
        value={value}
        onChange={e => onChange(e.target.value)}
        placeholder={placeholder}
        rows={rows ?? 3}
        className="w-full px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors resize-vertical font-mono"
      />
    </div>
  )
}

function ToggleField({ label, checked, onChange }: {
  label: string; checked: boolean; onChange: (v: boolean) => void
}) {
  return (
    <label className="flex items-center gap-3 cursor-pointer">
      <input
        type="checkbox"
        checked={checked}
        onChange={e => onChange(e.target.checked)}
        className="rounded border-gray-300 dark:border-gray-600 text-blue-600 focus:ring-blue-500 dark:bg-gray-700"
      />
      <span className="text-sm text-gray-700 dark:text-gray-300">{label}</span>
    </label>
  )
}

function maskApiKey(key: string): string {
  if (!key) return ''
  return key.length <= 4 ? '****' : `****${key.slice(-4)}`
}

function UsageSelect({ label, value, onChange, options, cap, notSelectedLabel, checkingLabel, availableLabel, notConfiguredLabel }: {
  label: string
  value: string
  onChange: (v: string) => void
  options: { value: string; label: string }[]
  cap: boolean | undefined
  notSelectedLabel: string
  checkingLabel: string
  availableLabel: string
  notConfiguredLabel: string
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">{label}</label>
      <div className="flex items-center gap-2">
        <select
          value={value}
          onChange={e => onChange(e.target.value)}
          className="flex-1 min-w-0 px-3 py-2 text-sm bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500 transition-colors"
        >
          <option value="">{notSelectedLabel}</option>
          {options.map(o => (
            <option key={o.value} value={o.value}>{o.label}</option>
          ))}
        </select>
        <span className={`shrink-0 text-xs ${cap ? 'text-green-600 dark:text-green-400' : 'text-gray-400 dark:text-gray-500'}`}>
          {cap === undefined ? checkingLabel : cap ? availableLabel : notConfiguredLabel}
        </span>
      </div>
    </div>
  )
}

function RowAction({ onClick, disabled, title, danger, children }: {
  onClick: () => void
  disabled?: boolean
  title?: string
  danger?: boolean
  children: React.ReactNode
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={title}
      className={`px-2 py-1 text-xs font-medium rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
        danger
          ? 'text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 hover:bg-red-100 dark:hover:bg-red-900/40'
          : 'text-gray-600 dark:text-gray-300 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700'
      }`}
    >
      {children}
    </button>
  )
}

function filterGuide(guide: string): string {
  const platform = navigator.platform.startsWith('Mac') ? 'macOS'
    : navigator.platform.startsWith('Win') ? 'Windows'
    : 'Linux'
  const prefix = `${platform}:`
  const line = guide.split('\n').find(l => l.startsWith(prefix))
  return line ? line.slice(prefix.length).trimStart() : guide
}

function formatSize(bytes: number): string {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}
