import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import AuthGate from './SignIn'
import './style.css'

// The shell opens the page as /?admin_token=… (the per-boot token the daemon
// wrote beside the db). Keep it for this tab only and take it off the URL so
// it never lands in history, screenshots or a copied link.
try {
  const params = new URLSearchParams(location.search)
  const token = params.get('admin_token')
  if (token) {
    sessionStorage.setItem('taisce.admin_token', token)
    params.delete('admin_token')
    const rest = params.toString()
    history.replaceState(null, '', location.pathname + (rest ? `?${rest}` : '') + location.hash)
  }
} catch {
  // storage blocked: admin calls will explain what to do (ApiError admin_token)
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {/* SERVER mode: a passkey sign-in before anything loads (LOCAL: passes straight through) */}
    <AuthGate>
      <App />
    </AuthGate>
  </React.StrictMode>,
)
