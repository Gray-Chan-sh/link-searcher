import { createContext, useContext, useState, useEffect, useCallback, type ReactNode } from 'react'
import zh from './zh'
import en from './en'
import ja from './ja'
import ko from './ko'
import { getConfig, updateConfig } from '../api/config'

type Lang = 'zh' | 'en' | 'ja' | 'ko'
const translations: Record<Lang, Record<string, string>> = { zh, en, ja, ko }

// 模块级当前语言 + 翻译函数：供无 React context 的模块（如 ai/streamStore
// 在用户切走后代为落库）取用户可见文案，避免硬编码中文。
let currentLang: Lang = 'zh'
export function setI18nLang(l: Lang) {
    currentLang = l
}
export function translate(key: string, params?: Record<string, string | number>): string {
    let str = translations[currentLang][key] ?? key
    if (params) {
        for (const [k, v] of Object.entries(params)) {
            str = str.replace(`{${k}}`, String(v))
        }
    }
    return str
}

interface I18nContextType {
    t: (key: string, params?: Record<string, string | number>) => string
    lang: Lang
    setLang: (lang: Lang) => void
}

const I18nContext = createContext<I18nContextType>({
    t: (k: string) => k,
    lang: 'zh',
    setLang: () => {},
})
export { I18nContext }

export function I18nProvider({ children }: { children: ReactNode }) {
    const [lang, setLangState] = useState<Lang>('zh')

    useEffect(() => {
        getConfig().then(c => {
            if (c.language === 'en' || c.language === 'zh' || c.language === 'ja' || c.language === 'ko') {
                setLangState(c.language as Lang)
            }
        }).catch(() => {})
    }, [])

    // 同步模块级语言，使非组件模块的 translate() 与界面一致。
    useEffect(() => {
        setI18nLang(lang)
    }, [lang])

    const setLang = useCallback(async (l: Lang) => {
        setLangState(l)
        await updateConfig({ language: l })
    }, [])

    const t = useCallback((key: string, params?: Record<string, string | number>) => {
        let str = translations[lang][key] ?? key
        if (params) {
            for (const [k, v] of Object.entries(params)) {
                str = str.replace(`{${k}}`, String(v))
            }
        }
        return str
    }, [lang])

    return (
        <I18nContext.Provider value={{ t, lang, setLang }}>
            {children}
        </I18nContext.Provider>
    )
}

export function useI18n() {
    return useContext(I18nContext)
}
