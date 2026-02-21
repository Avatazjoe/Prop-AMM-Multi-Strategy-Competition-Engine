import React, { Component, Suspense, lazy } from 'react'

const PropAMMSubmit = lazy(() => import('./PropAMMSubmit'))

class DashboardModuleBoundary extends Component {
  constructor(props) {
    super(props)
    this.state = { hasError: false, message: '' }
  }

  static getDerivedStateFromError(error) {
    return { hasError: true, message: error?.message || String(error) }
  }

  componentDidCatch(error) {
    console.error('Dashboard module error:', error)
  }

  render() {
    if (this.state.hasError) {
      return (
        <div style={{
          minHeight: '100vh',
          background: '#080c0f',
          color: '#ffb4b4',
          fontFamily: 'monospace',
          padding: '24px',
        }}>
          <h2 style={{ marginTop: 0 }}>Dashboard module failed to load</h2>
          <p style={{ whiteSpace: 'pre-wrap' }}>{this.state.message}</p>
          <p>Open the browser console for stack details.</p>
        </div>
      )
    }

    return this.props.children
  }
}

export default function App() {
  return (
    <DashboardModuleBoundary>
      <Suspense
        fallback={
          <div style={{
            minHeight: '100vh',
            background: '#080c0f',
            color: '#c8d8e8',
            fontFamily: 'monospace',
            padding: '24px',
          }}>
            Loading dashboard...
          </div>
        }
      >
        <PropAMMSubmit />
      </Suspense>
    </DashboardModuleBoundary>
  )
}
