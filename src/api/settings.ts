import { invoke } from '@tauri-apps/api/core'

export async function getSettings(): Promise<Record<string, string>> {
  return invoke<Record<string, string>>('get_settings')
}

export async function updateSettings(settings: Record<string, string>): Promise<void> {
  return invoke('update_settings', { settings })
}

export interface OcrEngineStatus {
    engine_type: string
    name: string
    available: boolean
    platforms: string[]
    install_guide: string
}

export interface OcrTestResult {
    success: boolean
    text: string
    duration_ms: number
    error: string | null
}

export async function listOcrEngines(): Promise<OcrEngineStatus[]> {
    return invoke<OcrEngineStatus[]>('list_ocr_engines')
}

export async function testOcrEngine(engineType: string): Promise<OcrTestResult> {
    return invoke<OcrTestResult>('test_ocr_engine', { engineType })
}

export async function checkTesseract(): Promise<boolean> {
  return invoke<boolean>('check_tesseract')
}

export interface DependencyStatus {
    name: string
    command: string
    available: boolean
    install_guide: string
}

export async function checkDependencies(): Promise<DependencyStatus[]> {
    return invoke<DependencyStatus[]>('check_dependencies')
}