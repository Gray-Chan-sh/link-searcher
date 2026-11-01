import * as client from './client'

/** Wire values are PascalCase (serde unit-enum variant names), NOT lowercase. */
export type ModelType = 'Embedding' | 'Llm' | 'Reranker' | 'Unknown'

export interface ModelInfo {
    id: string
    model_type: ModelType
    enabled?: boolean
}

export interface ProviderInfo {
    id: string
    name: string
    base_url: string
    api_key: string
    models: ModelInfo[]
}

export interface ProviderOutcome {
    id: string
    pull_error: string | null
}

export interface ProviderTest {
    ok: boolean
    detail: string
}

export interface ConfigInfo {
    data_dir: string
    language: string
    lo_binary_path: string
    ai_api_base: string
    ai_api_key: string
    embedding_api_base: string
    embedding_api_key: string
    embedding_model: string
    llm_api_base: string
    llm_api_key: string
    llm_model: string
    providers: ProviderInfo[]
    active_embedding_model_id: string
    active_llm_model_id: string
    active_reranker_model_id: string
    semantic_weight: number
}

export interface MigrationProgress {
    stage: string
    progress: number
}

export interface MigrationCompleted {
    message: string
}

export async function getConfig(): Promise<ConfigInfo> {
    return client.invoke<ConfigInfo>('get_config')
}

export async function updateConfig(config: Partial<ConfigInfo>): Promise<void> {
    if ('data_dir' in config && config.data_dir === '') {
        throw new Error('data_dir cannot be empty')
    }
    const current = await getConfig()
    return client.invoke('update_config', {
        newConfig: { ...current, ...config } as ConfigInfo,
    })
}

export async function migrateData(oldPath: string, newPath: string): Promise<string> {
    return client.invoke<string>('migrate_data', { oldPath, newPath })
}

export async function restartApp(): Promise<void> {
    return client.invoke('restart_app')
}

export async function addProvider(name: string, baseUrl: string, apiKey: string): Promise<ProviderOutcome> {
    return client.invoke<ProviderOutcome>('add_provider', { name, baseUrl, apiKey })
}

export async function updateProvider(id: string, name: string, baseUrl: string, apiKey: string): Promise<void> {
    await client.invoke('update_provider', { id, name, baseUrl, apiKey })
}

export async function deleteProvider(id: string): Promise<void> {
    await client.invoke('delete_provider', { id })
}

export async function refreshProviderModels(id: string): Promise<ModelInfo[]> {
    return client.invoke<ModelInfo[]>('refresh_provider_models', { id })
}

/** 语料的向量空间状态（后端 `CorpusState`，与库里的事实一一对应）。 */
export type CorpusState = 'empty' | 'current' | 'stale' | 'mixed' | 'legacy'

/** 切换模型的"能有多确定"（后端 `SwitchVerdict`）——决定对话框措辞。 */
export type SwitchVerdict =
  | 'unchanged'
  | 'empty'
  | 'different_local'
  | 'unknown'
  | 'legacy_unknown'

export interface SetActiveModelResult {
  kind: string
  model_id: string
  /** 是否真的换了模型（false = 没必要弹框） */
  changed: boolean
  corpus_state: CorpusState
  /** 必须重建才能恢复语义检索（stale / mixed） */
  rebuild_required: boolean
  /** 需要重建的内容数 / 块数（让用户知道代价） */
  pending_docs: number
  pending_chunks: number
  verdict: SwitchVerdict
}

export interface EmbeddingSwitchPreview {
  model_id: string
  changed: boolean
  corpus_state: CorpusState
  rebuild_required: boolean
  pending_docs: number
  pending_chunks: number
  verdict: SwitchVerdict
}

/** 切换前预览（不写配置）：切过去之后库里会是什么状态。 */
export async function previewEmbeddingSwitch(modelId: string): Promise<EmbeddingSwitchPreview> {
  return client.invoke<EmbeddingSwitchPreview>('preview_embedding_switch', { modelId })
}

export async function setActiveModel(
  kind: 'embedding' | 'llm' | 'reranker',
  modelId: string,
): Promise<SetActiveModelResult> {
  return client.invoke<SetActiveModelResult>('set_active_model', { kind, modelId })
}

export async function testProvider(baseUrl: string, apiKey: string): Promise<ProviderTest> {
    return client.invoke<ProviderTest>('test_provider', { baseUrl, apiKey })
}