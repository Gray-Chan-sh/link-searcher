import { invoke } from '@tauri-apps/api/core'

export interface ConfigInfo {
    data_dir: string
    language: string
    lo_binary_path: string
}

export async function getConfig(): Promise<ConfigInfo> {
    return invoke<ConfigInfo>('get_config')
}

export async function updateConfig(config: Partial<ConfigInfo>): Promise<void> {
    const current = await getConfig()
    return invoke('update_config', {
        newConfig: { ...current, ...config } as ConfigInfo,
    })
}

export async function migrateData(oldPath: string, newPath: string): Promise<string> {
    return invoke<string>('migrate_data', { oldPath, newPath })
}
