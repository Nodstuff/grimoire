import { describe, expect, it } from 'vitest'
import { THEME_KEY, applyThemePref, loadThemePref, nextThemePref, resolveTheme, storeThemePref } from './theme'

function mem() {
  const m = new Map<string, string>()
  return {
    m,
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
  }
}

describe('theme', () => {
  it('defaults to system; stores light/dark, clears for system', () => {
    const s = mem()
    expect(loadThemePref(s)).toBe('system')
    storeThemePref('dark', s)
    expect(s.m.get(THEME_KEY)).toBe('dark')
    expect(loadThemePref(s)).toBe('dark')
    storeThemePref('system', s)
    expect(s.m.has(THEME_KEY)).toBe(false)
    s.m.set(THEME_KEY, 'purple')
    expect(loadThemePref(s)).toBe('system')
  })
  it('blocked storage never throws', () => {
    const boom = {
      getItem: () => {
        throw new Error('denied')
      },
      setItem: () => {
        throw new Error('denied')
      },
      removeItem: () => {
        throw new Error('denied')
      },
    }
    expect(loadThemePref(boom)).toBe('system')
    expect(() => storeThemePref('light', boom)).not.toThrow()
  })
  it('system follows the OS; explicit choices win', () => {
    expect(resolveTheme('system', true)).toBe('dark')
    expect(resolveTheme('system', false)).toBe('light')
    expect(resolveTheme('light', true)).toBe('light')
    expect(resolveTheme('dark', false)).toBe('dark')
  })
  it('the toggle cycles system → light → dark → system', () => {
    expect(nextThemePref('system')).toBe('light')
    expect(nextThemePref('light')).toBe('dark')
    expect(nextThemePref('dark')).toBe('system')
  })
  it('system leaves <html> without data-theme; a choice sets it', () => {
    const attrs = new Map<string, string>()
    const root = {
      setAttribute: (k: string, v: string) => void attrs.set(k, v),
      removeAttribute: (k: string) => void attrs.delete(k),
    } as unknown as HTMLElement
    applyThemePref('dark', root)
    expect(attrs.get('data-theme')).toBe('dark')
    applyThemePref('system', root)
    expect(attrs.has('data-theme')).toBe(false)
  })
})
