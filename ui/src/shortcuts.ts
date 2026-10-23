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

export type ShortcutAction =
  | 'actions' // ⌘K — the actions palette
  | 'open' // ⌘O — open a doc or place
  | 'search' // ⌘⇧F (and ⌘F, ⌘P, ⌘S outside a doc) — full-text search
  | 'ask' // ⌘/ — search, in ask mode
  | 'settings' // ⌘, — settings
  | 'autosaved' // ⌘S inside the editor: say it autosaves
  | 'tree' // ⌘T — file tree
  | 'newdoc' // ⌘N — new doc
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
  if (!mod || e.altKey) return null
  const shift = e.shiftKey
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
    case 't':
      return 'tree'
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

/** The palette a palette shortcut toggles, for "same key again closes". */
export type PaletteKind = 'open' | 'actions' | 'search'
