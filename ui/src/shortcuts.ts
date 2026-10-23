// Global keyboard-shortcut scheme. Kept as a pure function so the mapping is
// unit-testable without a DOM: resolveShortcut maps a keydown into an action,
// App.tsx's handler dispatches it and preventDefaults exactly the combos that
// resolve (so ⌘O never opens the browser's file dialog and ⌘, never its
// preferences, while unmapped combos like ⌘⇧O or ⌘C keep their default).
//
// The three palettes: ⌘O open (docs + places), ⌘K actions, ⌘⇧F search.
// The editor binds no ⌘K (Tiptap's link mark has no shortcut), so ⌘K opens
// Actions from inside a doc too; its own keys (⌘B/I/E/U, ⌘⇧S strike,
// ⌘⇧7/8/9 lists, ⌘⇧B quote, ⌘↵) never resolve here.
//
// Browser-reserved chords never reach a page in Chrome or Safari on macOS,
// so nothing here is bound to them: ⌘W ⌘⇧W ⌘T ⌘⇧T ⌘N ⌘⇧N ⌘Q ⌘M ⌘H ⌘1–9
// ⌘⇧[ ⌘⇧] ⌘⌥←/→. Hence ⌘\ for the sidebar, ⌘⌥N for a new doc and ⌘⇧H for
// Home. ⌘T and ⌘N stay as unadvertised aliases because inside the Mac app's
// web view (no browser menu) they do arrive and old habits keep working.

export type ShortcutAction =
  | 'actions' // ⌘K — the actions palette
  | 'open' // ⌘O — open a doc or place
  | 'search' // ⌘⇧F (and ⌘F, ⌘P, ⌘S outside a doc) — full-text search
  | 'ask' // ⌘/ — search, in ask mode
  | 'settings' // ⌘, — settings
  | 'autosaved' // ⌘S inside the editor: say it autosaves
  | 'tree' // ⌘\ — show or hide the sidebar (⌘T alias in the Mac app)
  | 'newdoc' // ⌘⌥N — new doc (⌘N alias in the Mac app)
  | 'review' // ⌘⇧R — review queue
  | 'gardeners' // ⌘G — gardeners
  | 'reload' // ⌘R — reload
  | 'back' // ⌘[ — history back
  | 'forward' // ⌘] — history forward
  | 'home' // ⌘⇧H — home (⌘W is the browser's close-tab and never reaches the page)
  | 'capture' // ⌘⇧I — quick capture to Inbox (⌘I alone is the editor's italic)
  | 'help' // ? — shortcut cheatsheet (no modifier; never while typing)
  | 'escape' // Esc — dismiss palette

export interface KeyLike {
  key: string
  /** the physical key: ⌥ turns e.key into a symbol (⌥N is a dead key) */
  code?: string
  shiftKey: boolean
  metaKey: boolean
  ctrlKey: boolean
  altKey?: boolean
}

export interface KeyContext {
  /** focus is in the doc editor (ProseMirror) */
  inEditor?: boolean
  /** focus is in any editable field (input, textarea, contenteditable) */
  typing?: boolean
}

/** Map a keydown to an action, or null if it isn't one of ours. Case-folds
 * the key so shift-combos (⌘⇧R arrives as "R") resolve alongside their base
 * key. Alt combos are never ours (⌥ characters, the editor's ⌘⌥ keys). */
export function resolveShortcut(e: KeyLike, ctx: KeyContext = {}): ShortcutAction | null {
  const key = e.key.toLowerCase()
  if (key === 'escape') return 'escape'
  const mod = e.metaKey || e.ctrlKey
  // `?` opens the cheatsheet with no modifier, except while typing
  if (key === '?' && !mod) return ctx.typing || ctx.inEditor ? null : 'help'
  const shift = e.shiftKey
  // ⌘⌥N: the one alt chord we own, read from the physical key
  if (mod && e.altKey && !shift && (e.code === 'KeyN' || key === 'n')) return 'newdoc'
  if (!mod || e.altKey) return null
  switch (key) {
    case 'k':
      return shift ? null : 'actions'
    case 'o':
      return shift ? null : 'open'
    case 'f':
      return 'search'
    case 'p':
      return shift ? null : 'search'
    case 's':
      // ⌘⇧S is the editor's strikethrough; ⌘S in a doc means "save"
      if (shift) return null
      return ctx.inEditor ? 'autosaved' : 'search'
    case '/':
      return 'ask'
    case ',':
      return shift ? null : 'settings'
    case '\\':
    case 't':
      return shift ? null : 'tree'
    case 'n':
      return shift ? null : 'newdoc'
    case 'r':
      return shift ? 'review' : 'reload'
    case 'g':
      return 'gardeners'
    case 'i':
      return shift ? 'capture' : null
    case '[':
      return 'back'
    case ']':
      return 'forward'
    case 'h':
      // ⌘H alone is macOS's Hide; ⌘W closes the browser tab
      return shift ? 'home' : null
    default:
      return null
  }
}

/** The chords as the UI shows them (kbd hints, the cheatsheet, ⌘K rows). */
export const KEYS = {
  tree: '⌘\\',
  newdoc: '⌘⌥N',
  home: '⌘⇧H',
  capture: '⌘⇧I',
} as const

/** The palette a palette shortcut toggles, for "same key again closes". */
export type PaletteKind = 'open' | 'actions' | 'search'
