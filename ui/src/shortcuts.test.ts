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
    ['⌘K → commands', meta('k'), 'commands'],
    ['⌘O → open', meta('o'), 'open'],
    ['⌘P → search', meta('p'), 'search'],
    ['⌘S → search (alias)', meta('s'), 'search'],
    ['⌘F → search (alias)', meta('f'), 'search'],
    ['⌘T → tree', meta('t'), 'tree'],
    ['⌘N → newdoc', meta('n'), 'newdoc'],
    ['⌘⇧N → newcanvas', meta('N', true), 'newcanvas'],
    ['⌘R → reload', meta('r'), 'reload'],
    ['⌘⇧R → review', meta('R', true), 'review'],
    ['⌘G → gardeners', meta('g'), 'gardeners'],
    ['⌘[ → back', meta('['), 'back'],
    ['⌘] → forward', meta(']'), 'forward'],
    ['⌘W → home', meta('w'), 'home'],
    ['Esc → escape', { key: 'Escape', shiftKey: false, metaKey: false, ctrlKey: false }, 'escape'],
    ['? → help (no modifier)', { key: '?', shiftKey: true, metaKey: false, ctrlKey: false }, 'help'],
  ]
  for (const [name, ev, want] of cases) {
    it(name, () => expect(resolveShortcut(ev)).toBe(want))
  }

  it('ctrl is treated as mod (non-mac)', () => {
    expect(resolveShortcut(ctrl('k'))).toBe('commands')
    expect(resolveShortcut(ctrl('R', true))).toBe('review')
  })

  it('a plain letter without a modifier is not a shortcut', () => {
    expect(resolveShortcut({ key: 'n', shiftKey: false, metaKey: false, ctrlKey: false })).toBeNull()
    expect(resolveShortcut({ key: '/', shiftKey: false, metaKey: false, ctrlKey: false })).toBeNull()
    expect(resolveShortcut({ key: '/', shiftKey: false, metaKey: true, ctrlKey: false })).toBe('ask')
  })

  it('shift disambiguates N and R without collision', () => {
    expect(resolveShortcut(meta('n'))).toBe('newdoc')
    expect(resolveShortcut(meta('N', true))).toBe('newcanvas')
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
})
