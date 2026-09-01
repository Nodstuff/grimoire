// Global keyboard-shortcut scheme. Kept as a pure function so the mapping is
// unit-testable without a DOM: resolveShortcut maps a keydown into an action,
// App.tsx's handler dispatches it. Every mod combo is preventDefault'd by the
// caller — ⌘P (webview Print) and ⌘O (open-file dialog) would otherwise fire.

export type ShortcutAction =
  | 'commands' // ⌘K — operations palette
  | 'open' // ⌘O — open a doc by title
  | 'search' // ⌘P / ⌘S / ⌘F — content (FTS) search
  | 'tree' // ⌘T — file tree
  | 'newdoc' // ⌘N — new doc
  | 'newcanvas' // ⌘⇧N — new canvas
  | 'review' // ⌘⇧R — review queue
  | 'gardeners' // ⌘G — gardeners
  | 'reload' // ⌘R — reload
  | 'back' // ⌘[ — history back
  | 'forward' // ⌘] — history forward
  | 'home' // ⌘W — home
  | 'help' // ? — shortcut cheatsheet (no modifier; caller ignores it in inputs)
  | 'escape' // Esc — dismiss palette

export interface KeyLike {
  key: string
  shiftKey: boolean
  metaKey: boolean
  ctrlKey: boolean
}

/** Map a keydown to an action, or null if it isn't a shortcut. Case-folds the
 * key so shift-combos (⌘⇧R arrives as "R") resolve alongside their base key. */
export function resolveShortcut(e: KeyLike): ShortcutAction | null {
  const key = e.key.toLowerCase()
  if (key === 'escape') return 'escape'
  // `?` opens the cheatsheet with no modifier; the caller must ignore it while
  // an editable field is focused (so typing `?` in a doc still types `?`).
  if (key === '?') return 'help'
  const mod = e.metaKey || e.ctrlKey
  if (!mod) return null
  switch (key) {
    case 'k':
      return 'commands'
    case 'o':
      return 'open'
    case 'p':
    case 's':
    case 'f':
      return 'search'
    case 't':
      return 'tree'
    case 'n':
      return e.shiftKey ? 'newcanvas' : 'newdoc'
    case 'r':
      return e.shiftKey ? 'review' : 'reload'
    case 'g':
      return 'gardeners'
    case '[':
      return 'back'
    case ']':
      return 'forward'
    case 'w':
      return 'home'
    default:
      return null
  }
}
