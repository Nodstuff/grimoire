import { describe, expect, it } from 'vitest'
import { resolveShortcut, type KeyLike, type ShortcutAction } from './shortcuts'

const meta = (key: string, shiftKey = false): KeyLike => ({
  key,
  shiftKey,
  metaKey: true,
  ctrlKey: false,
})
const ctrl = (key: string, shiftKey = false): KeyLike => ({
  key,
  shiftKey,
  metaKey: false,
  ctrlKey: true,
})

describe('resolveShortcut', () => {
  const cases: [string, KeyLike, ShortcutAction | null][] = [
    ['⌘K → actions', meta('k'), 'actions'],
    ['⌘O → open', meta('o'), 'open'],
    ['⌘⇧F → search', meta('F', true), 'search'],
    ['⌘F → search (alias)', meta('f'), 'search'],
    ['⌘P → search (alias)', meta('p'), 'search'],
    ['⌘S → search outside a doc (alias)', meta('s'), 'search'],
    ['⌘/ → ask', meta('/'), 'ask'],
    ['⌘, → settings', meta(','), 'settings'],
    ['⌘T → tree', meta('t'), 'tree'],
    ['⌘N → newdoc', meta('n'), 'newdoc'],
    ['⌘R → reload', meta('r'), 'reload'],
    ['⌘⇧R → review', meta('R', true), 'review'],
    ['⌘G → gardeners', meta('g'), 'gardeners'],
    ['⌘[ → back', meta('['), 'back'],
    ['⌘] → forward', meta(']'), 'forward'],
    ['⌘⇧H → home', meta('H', true), 'home'],
    ['⌘H is not ours (macOS Hide)', meta('h'), null],
    ['⌘W is not ours (closes the tab)', meta('w'), null],
    ['Esc → escape', { key: 'Escape', shiftKey: false, metaKey: false, ctrlKey: false }, 'escape'],
    ['? → help (no modifier)', { key: '?', shiftKey: true, metaKey: false, ctrlKey: false }, 'help'],
  ]
  for (const [name, ev, want] of cases) {
    it(name, () => expect(resolveShortcut(ev)).toBe(want))
  }

  it('ctrl is treated as mod (non-mac)', () => {
    expect(resolveShortcut(ctrl('k'))).toBe('actions')
    expect(resolveShortcut(ctrl(','))).toBe('settings')
    expect(resolveShortcut(ctrl('R', true))).toBe('review')
  })

  it('a plain letter without a modifier is not a shortcut', () => {
    expect(resolveShortcut({ key: 'n', shiftKey: false, metaKey: false, ctrlKey: false })).toBeNull()
    expect(resolveShortcut({ key: '/', shiftKey: false, metaKey: false, ctrlKey: false })).toBeNull()
    expect(resolveShortcut({ key: '/', shiftKey: false, metaKey: true, ctrlKey: false })).toBe('ask')
  })

  it('⌘⇧I captures; plain ⌘I stays the editor italic', () => {
    expect(resolveShortcut(meta('I', true))).toBe('capture')
    expect(resolveShortcut(meta('i'))).toBeNull()
  })

  it('shift disambiguates N and R without collision', () => {
    expect(resolveShortcut(meta('n'))).toBe('newdoc')
    expect(resolveShortcut(meta('N', true))).toBeNull()
    expect(resolveShortcut(meta('r'))).toBe('reload')
    expect(resolveShortcut(meta('R', true))).toBe('review')
  })

  it('bare letters (no mod) are not shortcuts', () => {
    expect(resolveShortcut({ key: 'k', shiftKey: false, metaKey: false, ctrlKey: false })).toBeNull()
    expect(resolveShortcut({ key: 'p', shiftKey: false, metaKey: false, ctrlKey: false })).toBeNull()
  })

  it('Esc resolves without a modifier', () => {
    expect(resolveShortcut({ key: 'Escape', shiftKey: false, metaKey: false, ctrlKey: false })).toBe(
      'escape',
    )
  })

  it('unmapped mod combos return null', () => {
    expect(resolveShortcut(meta('q'))).toBeNull()
    expect(resolveShortcut(meta('x'))).toBeNull()
  })

  it('in the editor: ⌘K still opens Actions (the editor binds no ⌘K), ⌘S says autosaved', () => {
    const ed = { inEditor: true, typing: true }
    expect(resolveShortcut(meta('k'), ed)).toBe('actions')
    expect(resolveShortcut(meta('o'), ed)).toBe('open')
    expect(resolveShortcut(meta('F', true), ed)).toBe('search')
    expect(resolveShortcut(meta('s'), ed)).toBe('autosaved')
  })

  it('leaves the editor’s own keys alone', () => {
    const ed = { inEditor: true, typing: true }
    for (const [k, shift] of [['b', false], ['i', false], ['e', false], ['u', false], ['S', true], ['B', true], ['7', true], ['8', true], ['Enter', false], ['z', false]] as const) {
      expect(resolveShortcut(meta(k, shift), ed)).toBeNull()
    }
    expect(resolveShortcut({ ...meta('c'), altKey: true }, ed)).toBeNull() // ⌘⌥C code block
  })

  it('? opens help only when not typing', () => {
    const q = { key: '?', shiftKey: true, metaKey: false, ctrlKey: false }
    expect(resolveShortcut(q)).toBe('help')
    expect(resolveShortcut(q, { typing: true })).toBeNull()
    expect(resolveShortcut(q, { inEditor: true })).toBeNull()
  })

  it('unhandled variants are null, so the caller never preventDefaults them', () => {
    expect(resolveShortcut(meta('O', true))).toBeNull() // ⌘⇧O
    expect(resolveShortcut(meta('K', true))).toBeNull() // ⌘⇧K
    expect(resolveShortcut(meta('P', true))).toBeNull() // ⌘⇧P
    expect(resolveShortcut({ ...meta('o'), altKey: true })).toBeNull() // ⌘⌥O
    expect(resolveShortcut({ ...meta(','), altKey: true })).toBeNull()
    expect(resolveShortcut({ key: 'o', shiftKey: false, metaKey: false, ctrlKey: false })).toBeNull()
    expect(resolveShortcut({ key: ',', shiftKey: false, metaKey: false, ctrlKey: false })).toBeNull()
  })
})
