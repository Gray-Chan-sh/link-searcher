import { invoke } from '@tauri-apps/api/core'

export interface ConfigInfo {
    data_dir: string
    language: string
    lo_binary_path: string
    ai_api_base: string
    ai_api_key: string
    embedding_model: string
    llm_model: string
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
