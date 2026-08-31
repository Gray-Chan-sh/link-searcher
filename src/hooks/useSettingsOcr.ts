import { useState, useEffect } from 'react'
import { useI18n } from '../i18n'
import { alert } from '../utils/platform'
import { checkDependencies, checkBgeInstalled, installBge, installFunasr, listOcrEngines, testOcrEngine, type BgeStatus, type DependencyStatus, type OcrEngineStatus, type OcrTestResult } from '../api/settings'

export function useSettingsOcr() {
  const { t } = useI18n()
  const [ocrEngines, setOcrEngines] = useState<OcrEngineStatus[]>([])
  const [ocrTesting, setOcrTesting] = useState(false)
  const [ocrResult, setOcrResult] = useState<OcrTestResult | null>(null)
  const [deps, setDeps] = useState<DependencyStatus[]>([])
  const [funasrInstalling, setFunasrInstalling] = useState(false)
  const [bgeStatus, setBgeStatus] = useState<BgeStatus[] | null>(null)
  const [bgeInstalling, setBgeInstalling] = useState(false)

  useEffect(() => {
    listOcrEngines().then(setOcrEngines).catch(() => {})
  }, [])

  useEffect(() => {
    checkDependencies().then(setDeps).catch(() => {})
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

  const handleInstallFunasr = async () => {
    setFunasrInstalling(true)
    try {
      await installFunasr()
      await alert(t('funasr_install_done'), 'FunASR')
      checkDependencies().then(setDeps).catch(() => {})
    } catch (e) {
      await alert(e instanceof Error ? e.message : String(e), 'FunASR')
    } finally {
      setFunasrInstalling(false)
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
    ocrEngines, ocrTesting, ocrResult, deps,
    funasrInstalling, bgeStatus, bgeInstalling,
    setDeps, setBgeStatus,
    handleTestOcr, handleInstallFunasr, handleInstallBge,
  }
}
