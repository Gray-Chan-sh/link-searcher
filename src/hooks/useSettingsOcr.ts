import { useState, useEffect } from 'react'
import { alert } from '../utils/platform'
import { checkBgeInstalled, checkRerankInstalled, installBge, listOcrEngines, testOcrEngine, type BgeStatus, type RerankStatus, type OcrEngineStatus, type OcrTestResult } from '../api/settings'

export function useSettingsOcr() {
  const [ocrEngines, setOcrEngines] = useState<OcrEngineStatus[]>([])
  const [ocrTesting, setOcrTesting] = useState(false)
  const [ocrResult, setOcrResult] = useState<OcrTestResult | null>(null)
  const [bgeStatus, setBgeStatus] = useState<BgeStatus[] | null>(null)
  const [rerankStatus, setRerankStatus] = useState<RerankStatus[] | null>(null)
  const [bgeInstalling, setBgeInstalling] = useState(false)

  useEffect(() => {
    listOcrEngines().then(setOcrEngines).catch(() => {})
  }, [])

  useEffect(() => {
    checkBgeInstalled().then(setBgeStatus).catch(() => {})
    checkRerankInstalled().then(setRerankStatus).catch(() => {})
  }, [])

  const handleTestOcr = async (engineType: string) => {
    setOcrTesting(true)
    setOcrResult(null)
    try {
      const r = await testOcrEngine(engineType)
      setOcrResult(r)
    } catch (e) {
      setOcrResult({ success: false, text: '', duration_ms: 0, error: e instanceof Error ? e.message : String(e) })
    } finally {
      setOcrTesting(false)
    }
  }

  const handleInstallBge = async (modelName?: string) => {
    setBgeInstalling(true)
    try {
      await installBge(modelName)
      checkBgeInstalled().then(setBgeStatus).catch(() => {})
    } catch (e) {
      await alert(e instanceof Error ? e.message : String(e), 'BGE')
    } finally {
      setBgeInstalling(false)
    }
  }

  return {
    ocrEngines, ocrTesting, ocrResult,
    bgeStatus, rerankStatus, bgeInstalling,
    setBgeStatus, setRerankStatus,
    handleTestOcr, handleInstallBge,
  }
}
