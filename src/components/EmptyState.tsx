interface EmptyStateProps {
    icon?: React.ReactNode
    title: string
    description?: string
    action?: React.ReactNode
}

export default function EmptyState({ icon, title, description, action }: EmptyStateProps) {
    return (
        <div className="flex flex-col items-center justify-center py-16 text-center px-4">
            {icon && <div className="text-gray-300 dark:text-gray-700 mb-4">{icon}</div>}
            <p className="text-sm font-medium text-gray-500 dark:text-gray-400">{title}</p>
            {description && <p className="text-xs text-gray-400 dark:text-gray-500 mt-1">{description}</p>}
            {action && <div className="mt-4">{action}</div>}
        </div>
    )
}
