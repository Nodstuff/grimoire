// The page is served by the daemon and runs in two places: the Grimoire.app
// webview (Tauri) and any browser tab on localhost:7425. Only the webview
// has native dialogs and window dragging; everything here degrades to "not
// available" in a browser so callers can hide the command rather than fail.
//
// No @tauri-apps/api dependency: the shell injects `__TAURI_INTERNALS__`
// into the page, and the one command we need (the dialog plugin's Save
// sheet) is a single invoke. Which commands the page may call is decided by
// crates/shell/capabilities/main.json, not here.

interface TauriInternals {
  invoke: (cmd: string, args?: unknown) => Promise<unknown>
}

/** The injected bridge, if any (`window.__TAURI_INTERNALS__`; `window` is
 * `globalThis` in a browser, and the tests run without a DOM). */
function internals(): TauriInternals | undefined {
  return (globalThis as { __TAURI_INTERNALS__?: TauriInternals }).__TAURI_INTERNALS__
}

/** True inside Grimoire.app; false in a plain browser tab (and under vitest). */
export function inTauri(): boolean {
  return !!internals()
}

export interface SaveDialogOpts {
  title?: string
  /** a bare file name pre-fills the name; a folder opens there */
  defaultPath?: string
  filters?: { name: string; extensions: string[] }[]
}

/** The native "Save as…" sheet. Resolves to the chosen absolute path, or
 * null when the user cancels or the page is not inside the app. */
export async function saveDialog(opts: SaveDialogOpts): Promise<string | null> {
  const t = internals()
  if (!t) return null
  const r = (await t.invoke('plugin:dialog|save', { options: opts })) as string | { path?: string } | null
  if (!r) return null
  if (typeof r === 'string') return r
  return r.path ?? null
}

/** `grimoire-YYYY-MM-DD.db` — what the Save sheet proposes for a backup. */
export function backupFileName(now: Date = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0')
  return `grimoire-${now.getFullYear()}-${p(now.getMonth() + 1)}-${p(now.getDate())}.db`
}
