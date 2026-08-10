import { invoke } from '@tauri-apps/api/core'

/** Wire values are PascalCase (serde unit-enum variant names), NOT lowercase. */
export type ModelType = 'Embedding' | 'Llm' | 'Unknown'

export interface ModelInfo {
    id: string
    model_type: ModelType
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
}

export interface MigrationProgress {
    stage: string
    progress: number
}

export interface MigrationWarning {
    message: string
}

export interface MigrationCompleted {
    message: string
}

export async function getConfig(): Promise<ConfigInfo> {
    return invoke<ConfigInfo>('get_config')
}

export async function updateConfig(config: Partial<ConfigInfo>): Promise<void> {
    if ('data_dir' in config && config.data_dir === '') {
        throw new Error('data_dir cannot be empty')
    }
    const current = await getConfig()
    return invoke('update_config', {
        newConfig: { ...current, ...config } as ConfigInfo,
    })
}

export async function migrateData(oldPath: string, newPath: string): Promise<string> {
    return invoke<string>('migrate_data', { oldPath, newPath })
}

export async function restartApp(): Promise<void> {
    return invoke('restart_app')
}

export async function addProvider(name: string, baseUrl: string, apiKey: string): Promise<ProviderOutcome> {
    return invoke<ProviderOutcome>('add_provider', { name, baseUrl, apiKey })
}

export async function updateProvider(id: string, name: string, baseUrl: string, apiKey: string): Promise<void> {
    await invoke('update_provider', { id, name, baseUrl, apiKey })
}

export async function deleteProvider(id: string): Promise<void> {
    await invoke('delete_provider', { id })
}

export async function refreshProviderModels(id: string): Promise<ModelInfo[]> {
    return invoke<ModelInfo[]>('refresh_provider_models', { id })
}

export async function setActiveModel(kind: 'embedding' | 'llm', modelId: string): Promise<void> {
    await invoke('set_active_model', { kind, modelId })
}

export async function testProvider(baseUrl: string, apiKey: string): Promise<ProviderTest> {
    return invoke<ProviderTest>('test_provider', { baseUrl, apiKey })
}