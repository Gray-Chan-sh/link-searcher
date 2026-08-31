import { useState, useCallback } from 'react'
import { useI18n } from '../i18n'
import { confirm } from '../utils/platform'
import { addProvider, deleteProvider, getConfig, refreshProviderModels, setActiveModel, testProvider, updateConfig, type ConfigInfo, type ModelType, type ProviderInfo } from '../api/config'
import { aiCapabilities, type AiCapabilities } from '../api/files'
import { maskApiKey } from '../components/settings/SettingsFields'

export function useSettingsProviders(appConfig: ConfigInfo | null, setAppConfig: (c: ConfigInfo | null | ((prev: ConfigInfo | null) => ConfigInfo | null)) => void) {
  const { t } = useI18n()
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
  const [aiTest, setAiTest] = useState<{ kind: string; ok: boolean; detail: string }[] | null>(null)
  const [aiTestLoading, setAiTestLoading] = useState(false)

  const persistProviders = async (providers: ProviderInfo[]) => {
    if (!appConfig) return
    const updated = { ...appConfig, providers }
    await updateConfig(updated)
    setAppConfig(updated)
  }

  const handleSaveSemanticWeight = async () => {
    if (!appConfig) return
    try {
      await updateConfig(appConfig)
    } catch (e) {
      setAiWarn(e instanceof Error ? e.message : String(e))
    }
  }

  const setProvidersLocal = (updater: (ps: ProviderInfo[]) => ProviderInfo[]) => {
    setAppConfig((c: ConfigInfo | null) => (c ? { ...c, providers: updater(c.providers) } : c))
  }

  const providerInUse = (p: ProviderInfo) =>
    (appConfig?.active_embedding_model_id ?? '').startsWith(`${p.id}:`) ||
    (appConfig?.active_llm_model_id ?? '').startsWith(`${p.id}:`)

  const modelOptions = (kind: 'embedding' | 'llm', bgeStatus: { installed: boolean; model_dir: string; model_name: string }[] | null): { value: string; label: string }[] => {
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
    setCaps(c => (c ? { ...c, [kind]: undefined } : c))
    try {
      await setActiveModel(kind, modelId)
      aiCapabilities().then(setCaps).catch(() => {})
    } catch (e) {
      setAppConfig((c: ConfigInfo | null) => (c ? { ...c, [key]: prev } : c))
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
    const confirmed = await confirm(t('confirm_delete_provider', { name: p.name }), t('delete'))
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

  const testAi = useCallback(async () => {
    setAiTestLoading(true)
    setAiTest(null)
    try {
      const r = await aiCapabilities()
      setCaps(r)
      setAiTest([{ kind: 'embedding', ok: r.embedding, detail: r.embedding ? 'OK' : 'Not configured' },
                  { kind: 'llm', ok: r.llm, detail: r.llm ? 'OK' : 'Not configured' }])
    } catch (e) {
      setAiTest([{ kind: 'error', ok: false, detail: e instanceof Error ? e.message : String(e) }])
    } finally {
      setAiTestLoading(false)
    }
  }, [])

  return {
    caps, setCaps, aiWarn, setAiWarn,
    editingId, setEditingId, editDraft, setEditDraft,
    savingId, testingId, testOutcome, refreshingId, refreshMsg,
    adding, setAdding, newProv, setNewProv,
    modelFilter, setModelFilter, expandedGroups, setExpandedGroups,
    aiTest, aiTestLoading,
    persistProviders, handleSaveSemanticWeight, setProvidersLocal,
    providerInUse, modelOptions, modelInUse,
    handleToggleEnabled, handleActiveModel, handleTestProvider,
    handleRefreshProvider, handleDeleteProvider, openEdit,
    handleSaveEdit, handleModelType, handleAddProvider, testAi,
  }
}
