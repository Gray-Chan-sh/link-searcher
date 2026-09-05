import { useState, useEffect } from 'react'
import { alert } from '../utils/platform'
import { checkBgeInstalled, installBge, listOcrEngines, testOcrEngine, type BgeStatus, type OcrEngineStatus, type OcrTestResult } from '../api/settings'

export function useSettingsOcr() {
  const [ocrEngines, setOcrEngines] = useState<OcrEngineStatus[]>([])
  const [ocrTesting, setOcrTesting] = useState(false)
  const [ocrResult, setOcrResult] = useState<OcrTestResult | null>(null)
  const [bgeStatus, setBgeStatus] = useState<BgeStatus[] | null>(null)
  const [bgeInstalling, setBgeInstalling] = useState(false)

  useEffect(() => {
    listOcrEngines().then(setOcrEngines).catch(() => {})
  }, [])

  useEffect(() => {
    checkBgeInstalled().then(setBgeStatus).catch(() => {})
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

  const handleInstallBge = async () => {
    setBgeInstalling(true)
    try {
      await installBge()
      checkBgeInstalled().then(setBgeStatus).catch(() => {})
    } catch (e) {
      await alert(e instanceof Error ? e.message : String(e), 'BGE')
    } finally {
      setBgeInstalling(false)
    }
  }

  return {
    ocrEngines, ocrTesting, ocrResult,
    bgeStatus, bgeInstalling,
    setBgeStatus,
    handleTestOcr, handleInstallBge,
  }
}
