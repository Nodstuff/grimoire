import { describe, expect, it } from 'vitest'
import { bookCss, DEFAULTS, layoutFor, loadSettings, minutesLeft, percentOf, saveSettings, step, FONT_SIZES } from './settings'

function memStore() {
  const m = new Map<string, string>()
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) }
}

describe('reader settings', () => {
  it('round-trip per browser, and refuse values off the ladders', () => {
    const s = memStore()
    expect(loadSettings(s)).toEqual(DEFAULTS)
    saveSettings({ ...DEFAULTS, paper: 'sepia', fontSize: 120, flow: 'scrolled' }, s)
    expect(loadSettings(s)).toEqual({ ...DEFAULTS, paper: 'sepia', fontSize: 120, flow: 'scrolled' })
    s.setItem('taisce.reader.settings', JSON.stringify({ paper: 'neon', fontSize: 999, lineHeight: 'x' }))
    expect(loadSettings(s)).toEqual(DEFAULTS)
    s.setItem('taisce.reader.settings', '{not json')
    expect(loadSettings(s)).toEqual(DEFAULTS)
  })

  it('survives storage that throws', () => {
    const bad = {
      getItem: () => {
        throw new Error('no')
      },
      setItem: () => {
        throw new Error('no')
      },
    }
    expect(loadSettings(bad)).toEqual(DEFAULTS)
    expect(() => saveSettings(DEFAULTS, bad)).not.toThrow()
  })

  it('steps along a ladder and stops at its ends', () => {
    expect(step(FONT_SIZES, 100, 1)).toBe(110)
    expect(step(FONT_SIZES, 80, -1)).toBe(80)
    expect(step(FONT_SIZES, 170, 1)).toBe(170)
  })

  it('margins set the paginator; the phone keeps room for its bar', () => {
    expect(layoutFor('wide', false).gap).toBe('10%')
    expect(Number.parseInt(layoutFor('normal', true).margin)).toBeGreaterThanOrEqual(52)
  })

  it('the section CSS carries the paper as values and Newsreader when chosen', () => {
    const c = { paper: '#f4ecd8', ink: '#3b2f1f', link: '#7a4b16', mark: 'rgba(0,0,0,.2)', scheme: 'light' as const }
    const css = bookCss({ ...DEFAULTS, fontSize: 120 }, c, { normal: 'https://x/n.woff2', italic: 'https://x/i.woff2' })
    expect(css).toContain('background: #f4ecd8 !important')
    expect(css).toContain('font-size: 120% !important')
    expect(css).toContain("src: url(\"https://x/n.woff2\")")
    expect(bookCss({ ...DEFAULTS, face: 'book' }, c, { normal: 'n', italic: 'i' })).not.toContain('@font-face')
    expect(css).not.toMatch(/var\(--/)
  })

  it('says how long is left in the chapter, and the percent', () => {
    expect(minutesLeft(12.4)).toBe('12 min left in chapter')
    expect(minutesLeft(0.3)).toBe('Less than a minute left in chapter')
    expect(minutesLeft(0)).toBeNull()
    expect(minutesLeft(undefined)).toBeNull()
    expect(percentOf(0.574)).toBe(57)
    expect(percentOf(1.3)).toBe(100)
    expect(percentOf(Number.NaN)).toBe(0)
  })
})
