import { Component, type ReactNode } from 'react'

interface ErrorBoundaryProps {
  children: ReactNode
}

interface ErrorBoundaryState {
  hasError: boolean
  error: Error | null
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { hasError: false, error: null }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { hasError: true, error }
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error('[ErrorBoundary]', error, info.componentStack)
  }

  render() {
    if (this.state.hasError) {
      return (
        <div className="p-6 text-sm">
          <div className="text-red-600 dark:text-red-400 font-medium mb-2">应用出错了</div>
          <div className="text-gray-700 dark:text-gray-300 font-mono text-xs whitespace-pre-wrap max-h-40 overflow-auto mb-2">
            {this.state.error?.message || String(this.state.error)}
          </div>
          <div className="text-gray-500 dark:text-gray-400 text-xs font-mono whitespace-pre-wrap max-h-60 overflow-auto">
            {this.state.error?.stack}
          </div>
          <button
            onClick={() => this.setState({ hasError: false, error: null })}
            className="mt-3 px-3 py-1 text-xs text-white bg-purple-600 hover:bg-purple-700 rounded transition-colors"
          >
            重试
          </button>
        </div>
      )
    }
    return this.props.children
  }
}
