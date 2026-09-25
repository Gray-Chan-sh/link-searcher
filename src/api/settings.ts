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
    /** Local model directory name, e.g. `bge-large-zh-v1.5`. */
    model_id: string
}

export async function installBge(modelName?: string): Promise<void> {
    return client.invoke('install_bge', { modelName: modelName ?? null })
}

export async function checkBgeInstalled(): Promise<BgeStatus[]> {
    return client.invoke<BgeStatus[]>('check_bge_installed')
}

export interface RerankStatus {
    installed: boolean
    model_name: string
    /** Local model directory name, i.e. suffix of `local:<model_id>`. */
    model_id: string
}

export async function checkRerankInstalled(): Promise<RerankStatus[]> {
    return client.invoke<RerankStatus[]>('check_rerank_installed')
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

// ── Performance ──

export interface HardwareInfo {
    cpu_cores: number
    disk_speed_mbps: number
    disk_type: string
    platform: string
}

export interface PerformanceProfile {
    batch_io_concurrency: number
    commit_interval: number
    writer_buffer_mb: number
    tier: string
}

export async function detectHardware(): Promise<HardwareInfo> {
    return client.invoke<HardwareInfo>('detect_hardware')
}

export async function autoOptimize(): Promise<PerformanceProfile> {
    return client.invoke<PerformanceProfile>('auto_optimize')
}

export async function getPerformanceProfile(): Promise<PerformanceProfile> {
    return client.invoke<PerformanceProfile>('get_performance_profile')
}