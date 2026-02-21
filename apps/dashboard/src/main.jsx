import React, { Component } from 'react'
import { createRoot } from 'react-dom/client'
import App from './App'
import './styles.css'

class RootErrorBoundary extends Component {
	constructor(props) {
		super(props)
		this.state = { hasError: false, message: '' }
	}

	static getDerivedStateFromError(error) {
		return { hasError: true, message: error?.message || String(error) }
	}

	componentDidCatch(error) {
		console.error('Dashboard runtime error:', error)
	}

	render() {
		if (this.state.hasError) {
			return (
				<div style={{
					minHeight: '100vh',
					background: '#080c0f',
					color: '#ffb4b4',
					padding: '24px',
					fontFamily: 'monospace',
				}}>
					<h2 style={{ marginTop: 0 }}>Dashboard runtime error</h2>
					<p style={{ whiteSpace: 'pre-wrap' }}>{this.state.message}</p>
					<p>Check browser console for details.</p>
				</div>
			)
		}
		return this.props.children
	}
}

createRoot(document.getElementById('root')).render(
	<RootErrorBoundary>
		<App />
	</RootErrorBoundary>
)
