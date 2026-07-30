import { useState } from 'react'

interface Step {
    title: string
    description: string
    action: string
}

const steps: Step[] = [
    { title: '添加资料库', description: '选择要搜索的文件夹，可以将整个磁盘或指定目录添加进来', action: '打开资料库页面' },
    { title: '构建索引', description: '扫描文件并建立全文索引，搜索文件内容（不仅仅是文件名）', action: '开始扫描' },
    { title: '搜索文件', description: '输入关键词，立刻找到包含该内容的文件，支持中文分词', action: '去搜索' },
]

export default function OnboardingWizard({ onClose }: { onClose: () => void }) {
    const [step, setStep] = useState(0)

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
                    <button onClick={onClose} className="flex-1 px-4 py-2 text-sm text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800 rounded-lg">Skip</button>
                    <button onClick={() => step < 2 ? setStep(s => s + 1) : onClose()} className="flex-1 px-4 py-2 text-sm text-white bg-blue-600 hover:bg-blue-700 rounded-lg">
                        {step < 2 ? 'Next' : 'Get Started'}
                    </button>
                </div>
            </div>
        </div>
    )
}
