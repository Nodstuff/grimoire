import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import AuthGate from './SignIn'
import '@fontsource-variable/geist'
import '@fontsource-variable/geist-mono'
import '@fontsource-variable/newsreader/opsz.css'
import '@fontsource-variable/newsreader/opsz-italic.css'
import './lapis/tokens.css'
import './lapis/lapis.css'
import './style.css'
import './shell/shell.css'
import { applyThemePref, getThemePref } from './lapis/theme'
import { legacyRedirect } from './router'
import { inTauri } from './tauri'

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

// the old query deep links (?doc=, ?block=, ?tab=) become real paths
try {
  const to = legacyRedirect(location.pathname, location.search)
  if (to) history.replaceState(null, '', to + location.hash)
} catch {
  // a malformed URL: boot where we are
}

// theme before first paint; the Mac app's window chrome needs room for the
// traffic lights (shell.css keys on .tauri)
applyThemePref(getThemePref())
if (inTauri()) document.documentElement.classList.add('tauri')

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {/* SERVER mode: a passkey sign-in before anything loads (LOCAL: passes straight through) */}
    <AuthGate>
      <App />
    </AuthGate>
  </React.StrictMode>,
)
