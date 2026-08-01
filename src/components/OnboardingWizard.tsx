import { useState } from 'react'
import { useI18n } from '../i18n'

export default function OnboardingWizard({ onClose }: { onClose: () => void }) {
    const { t } = useI18n()
    const [step, setStep] = useState(0)

    const steps: { title: string; description: string }[] = [
        { title: t('ob_step1_title'), description: t('ob_step1_desc') },
        { title: t('ob_step2_title'), description: t('ob_step2_desc') },
        { title: t('ob_step3_title'), description: t('ob_step3_desc') },
    ]

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
            <div className="bg-white dark:bg-gray-900 rounded-2xl shadow-2xl p-8 max-w-md w-full mx-4">
                <div className="flex justify-between mb-6">
                    {steps.map((_, i) => (
                        <div key={i} className={`h-1 flex-1 mx-1 rounded-full ${i <= step ? 'bg-blue-600' : 'bg-gray-200 dark:bg-gray-700'}`} />
                    ))}
                </div>
                <div className="text-center mb-8">
                    <div className="text-5xl mb-4">{['📁', '🔍', '🎉'][step]}</div>
                    <h3 className="text-lg font-semibold text-gray-900 dark:text-gray-100 mb-2">{steps[step].title}</h3>
                    <p className="text-sm text-gray-500 dark:text-gray-400">{steps[step].description}</p>
                </div>
                <div className="flex gap-3">
                    <button onClick={onClose} className="flex-1 px-4 py-2 text-sm text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 rounded-lg">{t('skip')}</button>
                    <button onClick={() => step < 2 ? setStep(s => s + 1) : onClose()} className="flex-1 px-4 py-2 text-sm text-white bg-blue-600 hover:bg-blue-700 rounded-lg">
                        {step < 2 ? t('next') : t('get_started')}
                    </button>
                </div>
            </div>
        </div>
    )
}
