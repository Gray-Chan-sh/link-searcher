import { useI18n } from '../../i18n'
import { LoadingSpinner, PlusIcon } from '../../icons'
import type { ConfigInfo, ModelType, ProviderInfo } from '../../api/config'
import type { AiCapabilities } from '../../api/files'
import type { BgeStatus } from '../../api/settings'
import { Section, UsageSelect, RowAction, maskApiKey } from './SettingsFields'

interface AiTabProps {
  appConfig: ConfigInfo | null
  setAppConfig: (c: ConfigInfo | null | ((prev: ConfigInfo | null) => ConfigInfo | null)) => void
  caps: AiCapabilities | null
  aiWarn: string | null
  bgeStatus: BgeStatus[] | null
  bgeInstalling: boolean
  editingId: string | null
  editDraft: { name: string; baseUrl: string; apiKey: string; keyTouched: boolean; reveal: boolean } | null
  savingId: string | null
  testingId: string | null
  testOutcome: { id: string; ok: boolean; detail: string } | null
  refreshingId: string | null
  refreshMsg: { id: string; text: string; isError: boolean } | null
  adding: boolean
  newProv: { name: string; baseUrl: string; apiKey: string }
  modelFilter: Record<string, string>
  expandedGroups: Set<string>
  aiTest: { kind: string; ok: boolean; detail: string }[] | null
  aiTestLoading: boolean
  onSaveSemanticWeight: () => void
  onActiveModel: (kind: 'embedding' | 'llm', modelId: string) => void
  onTestProvider: (p: ProviderInfo) => void
  onRefreshProvider: (p: ProviderInfo) => void
  onDeleteProvider: (p: ProviderInfo) => void
  onOpenEdit: (p: ProviderInfo) => void
  onSaveEdit: (p: ProviderInfo) => void
  onModelType: (p: ProviderInfo, modelId: string, modelType: ModelType) => void
  onToggleEnabled: (p: ProviderInfo, modelId: string, enabled: boolean) => void
  onAddProvider: () => void
  onTestAi: () => void
  onInstallBge: () => void
  providerInUse: (p: ProviderInfo) => boolean
  modelInUse: (p: ProviderInfo, modelId: string) => boolean
  modelOptions: (kind: 'embedding' | 'llm') => { value: string; label: string }[]
  setEditingId: (id: string | null) => void
  setEditDraft: (d: { name: string; baseUrl: string; apiKey: string; keyTouched: boolean; reveal: boolean } | null | ((prev: { name: string; baseUrl: string; apiKey: string; keyTouched: boolean; reveal: boolean } | null) => { name: string; baseUrl: string; apiKey: string; keyTouched: boolean; reveal: boolean } | null)) => void
  setAdding: (v: boolean) => void
  setNewProv: (v: { name: string; baseUrl: string; apiKey: string }) => void
  setModelFilter: (updater: (f: Record<string, string>) => Record<string, string>) => void
  setExpandedGroups: (updater: (s: Set<string>) => Set<string>) => void
}

export function AiTab({
  appConfig, setAppConfig, caps, aiWarn, bgeStatus, bgeInstalling,
  editingId, editDraft, savingId, testingId, testOutcome, refreshingId, refreshMsg,
  adding, newProv, modelFilter, expandedGroups, aiTest, aiTestLoading,
  onSaveSemanticWeight, onActiveModel, onTestProvider, onRefreshProvider, onDeleteProvider,
  onOpenEdit, onSaveEdit, onModelType, onToggleEnabled, onAddProvider, onTestAi, onInstallBge,
  providerInUse, modelInUse, modelOptions,
  setEditingId, setEditDraft, setAdding, setNewProv, setModelFilter, setExpandedGroups,
}: AiTabProps) {
  const { t } = useI18n()

  return (
    <div className="space-y-6">
      <Section title={t('ai_service')}>
        <p className="text-xs text-gray-500 dark:text-gray-400 mb-3">{t('ai_service_desc')}</p>
        {aiWarn && (
          <div className="px-3 py-2 text-xs text-amber-700 dark:text-amber-400 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-900 rounded-lg">
            {aiWarn}
          </div>
        )}

        <div className="text-xs font-semibold text-gray-700 dark:text-gray-300">{t('ai_current_usage')}</div>
        <div className="p-3 bg-gray-50 dark:bg-gray-800/40 border border-gray-200 dark:border-gray-700 rounded-lg">
          <label className="block text-xs font-medium text-gray-600 dark:text-gray-300 mb-1">
            {t('retrieval_strategy')} — {t('semantic_weight_label')}
          </label>
          <input
            type="range"
            min="0" max="1" step="0.05"
            value={appConfig?.semantic_weight ?? 0.3}
            onChange={e => { const v = Number(e.target.value); setAppConfig((c: ConfigInfo | null) => c ? { ...c, semantic_weight: v } : c) }}
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
            onClick={onSaveSemanticWeight}
            className="mt-2 px-2 py-0.5 text-[10px] font-medium text-white bg-purple-600 hover:bg-purple-700 rounded transition-colors"
          >
            {t('save')}
          </button>
        </div>
        <div className="space-y-3">
          <UsageSelect
            label={t('embedding_model')}
            value={appConfig?.active_embedding_model_id ?? ''}
            onChange={v => onActiveModel('embedding', v)}
            options={modelOptions('embedding')}
            cap={caps?.embedding}
            notSelectedLabel={t('ai_not_selected')}
            checkingLabel={t('ai_checking')}
            availableLabel={t('ai_available')}
            notConfiguredLabel={t('ai_not_configured')}
          />
          {bgeStatus && !bgeStatus.some(s => s.installed) && (
            <button
              onClick={onInstallBge}
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
            onChange={v => onActiveModel('llm', v)}
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
            onClick={onTestAi}
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
                    <RowAction onClick={() => onTestProvider(p)} disabled={testingId === p.id}>
                      {testingId === p.id ? '…' : t('ai_test')}
                    </RowAction>
                    <RowAction onClick={() => onOpenEdit(p)}>{t('ai_edit')}</RowAction>
                    <RowAction onClick={() => onRefreshProvider(p)} disabled={refreshingId === p.id}>
                      {refreshingId === p.id ? '…' : t('refresh')}
                    </RowAction>
                    <RowAction danger onClick={() => onDeleteProvider(p)} disabled={providerInUse(p)} title={providerInUse(p) ? t('ai_delete_in_use') : undefined}>
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
                                            onChange={e => onModelType(p, m.id, e.target.value as ModelType)}
                                            className="px-1.5 py-0.5 text-xs bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-blue-500 transition-colors"
                                          >
                                            <option value="Embedding">{t('ai_type_embedding')}</option>
                                            <option value="Llm">{t('ai_type_llm')}</option>
                                            <option value="Unknown">{t('ai_type_unknown')}</option>
                                          </select>
                                          <button
                                            type="button"
                                            onClick={() => onToggleEnabled(p, m.id, false)}
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
                            <input
                              type="text"
                              value={modelFilter[p.id] ?? ''}
                              onChange={e => setModelFilter(f => ({ ...f, [p.id]: e.target.value }))}
                              placeholder={t('model_filter_placeholder')}
                              className="mb-1.5 w-full px-2 py-1 text-xs bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded text-gray-900 dark:text-gray-100 placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500 transition-colors"
                            />
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
                                            onClick={() => onToggleEnabled(p, m.id, true)}
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
                            onClick={() => setEditDraft((d: { name: string; baseUrl: string; apiKey: string; keyTouched: boolean; reveal: boolean } | null) => {
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
                          onClick={() => onSaveEdit(p)}
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
                onClick={onAddProvider}
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
  )
}
