// reladraw fences: source → SVG entirely in the browser (the package is pure
// JS, no DOM, no network), so diagrams render offline like mermaid.

export type AppTheme = 'dark' | 'light'

type Reladraw = typeof import('reladraw')

// ~600KB: fetched on the first reladraw fence, not at boot
let reladrawMod: Promise<Reladraw> | null = null
function loadReladraw() {
  if (!reladrawMod) {
    reladrawMod = import('reladraw')
    reladrawMod.catch(() => {
      reladrawMod = null // let the next fence retry the fetch
    })
  }
  return reladrawMod
}

/** The app is dark unless the root opts into a light color-scheme. */
export function appTheme(): AppTheme {
  if (typeof document === 'undefined') return 'dark'
  return getComputedStyle(document.documentElement).colorScheme.trim() === 'light' ? 'light' : 'dark'
}

// a theme passed in beats the file's own `diagram theme: nord`, so the app's
// theme is only the default for a file that names none
const NAMES_THEME = /^\s*diagram\b[^\n]*\btheme\s*:/m

export type ReladrawResult = { svg: string } | { err: string }

export async function renderReladraw(code: string, theme: AppTheme): Promise<ReladrawResult> {
  const r = await loadReladraw()
  try {
    const svg = NAMES_THEME.test(code) ? r.compile(code) : r.compile(code, { theme: r.THEMES[theme] })
    return { svg }
  } catch (e) {
    // `line 12: two placements for "server"`, the CLI's form
    if (e instanceof r.SourceError) return { err: e.format() }
    return { err: String(e).split('\n')[0] }
  }
}
