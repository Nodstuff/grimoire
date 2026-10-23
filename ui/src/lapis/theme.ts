// Theme: system (follow prefers-color-scheme), light or dark. The choice is
// persisted per browser (localStorage, every access in try/catch: private
// windows and blocked storage just fall back to system). `system` leaves
// <html> without a data-theme attribute so the CSS media query decides; an
// explicit choice sets data-theme=light|dark (tokens.css keys on both).

import { useCallback, useEffect, useSyncExternalStore } from 'react'

export type ThemePref = 'system' | 'light' | 'dark'
export type Theme = 'light' | 'dark'

export const THEME_KEY = 'taisce.theme'
export const THEME_PREFS: ThemePref[] = ['system', 'light', 'dark']

export function isThemePref(v: unknown): v is ThemePref {
  return v === 'system' || v === 'light' || v === 'dark'
}

export function loadThemePref(storage: Pick<Storage, 'getItem'> | null = safeStorage()): ThemePref {
  try {
    const v = storage?.getItem(THEME_KEY)
    return isThemePref(v) ? v : 'system'
  } catch {
    return 'system'
  }
}

export function storeThemePref(pref: ThemePref, storage: Pick<Storage, 'setItem' | 'removeItem'> | null = safeStorage()) {
  try {
    if (pref === 'system') storage?.removeItem(THEME_KEY)
    else storage?.setItem(THEME_KEY, pref)
  } catch {
    /* storage blocked: the choice lasts this page */
  }
}

/** What a preference shows, given whether the OS is dark. */
export function resolveTheme(pref: ThemePref, systemDark: boolean): Theme {
  if (pref === 'system') return systemDark ? 'dark' : 'light'
  return pref
}

/** The toggle's next step: system → light → dark → system. A toggle that
 * only flips would strand people on an explicit choice with no way back to
 * "follow the OS"; Settings › Appearance offers all three directly. */
export function nextThemePref(pref: ThemePref): ThemePref {
  return THEME_PREFS[(THEME_PREFS.indexOf(pref) + 1) % THEME_PREFS.length]
}

export const THEME_LABEL: Record<ThemePref, string> = { system: 'System', light: 'Light', dark: 'Dark' }

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage
  } catch {
    return null
  }
}

function systemDark(): boolean {
  try {
    return typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches
  } catch {
    return false
  }
}

/** Put a preference on <html>. Called once at boot (main.tsx) and on change. */
export function applyThemePref(pref: ThemePref, root: HTMLElement | null = typeof document === 'undefined' ? null : document.documentElement) {
  if (!root) return
  if (pref === 'system') root.removeAttribute('data-theme')
  else root.setAttribute('data-theme', pref)
}

/* ---------- a tiny shared store, so every toggle on the page agrees ---------- */

let current: ThemePref = loadThemePref()
const listeners = new Set<() => void>()

export function getThemePref(): ThemePref {
  return current
}

export function setThemePref(pref: ThemePref) {
  current = pref
  storeThemePref(pref)
  applyThemePref(pref)
  for (const l of listeners) l()
}

function subscribe(l: () => void) {
  listeners.add(l)
  return () => {
    listeners.delete(l)
  }
}

/** The theme preference, what it resolves to right now, and a setter. */
export function useTheme(): { pref: ThemePref; theme: Theme; setPref: (p: ThemePref) => void; cycle: () => void } {
  const pref = useSyncExternalStore(subscribe, getThemePref, () => 'system' as ThemePref)
  const dark = useSyncExternalStore(subscribeSystem, systemDark, () => false)
  const setPref = useCallback((p: ThemePref) => setThemePref(p), [])
  const cycle = useCallback(() => setThemePref(nextThemePref(getThemePref())), [])
  // another tab changed it: follow
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key !== THEME_KEY) return
      const v = isThemePref(e.newValue) ? e.newValue : 'system'
      current = v
      applyThemePref(v)
      for (const l of listeners) l()
    }
    window.addEventListener('storage', onStorage)
    return () => window.removeEventListener('storage', onStorage)
  }, [])
  return { pref, theme: resolveTheme(pref, dark), setPref, cycle }
}

function subscribeSystem(l: () => void) {
  try {
    const m = matchMedia('(prefers-color-scheme: dark)')
    m.addEventListener('change', l)
    return () => m.removeEventListener('change', l)
  } catch {
    return () => {}
  }
}
