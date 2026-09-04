import * as client from './client'

export async function getSettings(): Promise<Record<string, string>> {
  return client.invoke<Record<string, string>>('get_settings')
}

export async function updateSettings(settings: Record<string, string>): Promise<void> {
  return client.invoke('update_settings', { settings })
}

export interface OcrEngineStatus {
    engine_type: string
    name: string
    available: boolean
    platforms: string[]
    install_guide: string
    detail: string
}

export interface OcrTestResult {
    success: boolean
    text: string
    duration_ms: number
    error: string | null
}

export async function listOcrEngines(): Promise<OcrEngineStatus[]> {
    return client.invoke<OcrEngineStatus[]>('list_ocr_engines')
}

export async function testOcrEngine(engineType: string): Promise<OcrTestResult> {
    return client.invoke<OcrTestResult>('test_ocr_engine', { engineType })
}

export async function checkTesseract(): Promise<boolean> {
  return client.invoke<boolean>('check_tesseract')
}

export interface DependencyStatus {
    name: string
    command: string
    available: boolean
    install_guide: string
}

export async function checkDependencies(): Promise<DependencyStatus[]> {
    return client.invoke<DependencyStatus[]>('check_dependencies')
}

export async function getVersion(): Promise<{ hash: string; time: string }> {
    return client.invoke<{ hash: string; time: string }>('get_version')
}

export interface FunasrInstallResult {
    success: boolean
    message: string
}

export async function installFunasr(): Promise<void> {
    return client.invoke('install_funasr')
}

export interface BgeStatus {
    installed: boolean
    model_dir: string
    model_name: string
}

export async function installBge(modelName?: string): Promise<void> {
    return client.invoke('install_bge', { modelName: modelName ?? null })
}

export async function checkBgeInstalled(): Promise<BgeStatus[]> {
    return client.invoke<BgeStatus[]>('check_bge_installed')
}

// ── Dependency center / first-run setup ──

export interface DepStatus {
    id: string
    name: string
    available: boolean
    recommended: boolean
    size_bytes: number
    hint: string
    required_files: string[]
}

export interface SetupStatus {
    deps: DepStatus[]
    all_recommended_ready: boolean
    data_dir: string
}

export async function getSetupStatus(): Promise<SetupStatus> {
    return client.invoke<SetupStatus>('get_setup_status')
}

export interface DepInstallResult {
    dep: string
    success: boolean
    message: string
}

export async function installDep(dep: string): Promise<void> {
    return client.invoke('install_dep', { dep })
}

export async function cancelDepInstall(): Promise<void> {
    return client.invoke('cancel_dep_install')
}

export async function depInstallStatus(): Promise<{ installing: boolean; dep: string | null }> {
    return client.invoke('dep_install_status')
}