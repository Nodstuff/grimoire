// The reader's settings, per browser (localStorage, every access in
// try/catch), and the CSS they put into each section of the book.

import { safeStorage } from './sync'

export type Paper = 'auto' | 'light' | 'dark' | 'sepia'
export type Flow = 'paginated' | 'scrolled'
export type Margins = 'narrow' | 'normal' | 'wide'
export type Face = 'newsreader' | 'book'

export interface ReaderSettings {
  /** auto = the app's own light or dark */
  paper: Paper
  /** percent of the book's base size */
  fontSize: number
  lineHeight: number
  margins: Margins
  flow: Flow
  face: Face
}

export const DEFAULTS: ReaderSettings = { paper: 'auto', fontSize: 100, lineHeight: 1.55, margins: 'normal', flow: 'paginated', face: 'newsreader' }
export const FONT_SIZES = [80, 90, 100, 110, 120, 135, 150, 170]
export const LINE_HEIGHTS = [1.3, 1.45, 1.55, 1.7, 1.9]

const KEY = 'taisce.reader.settings'

const pick = <T,>(v: unknown, ok: readonly T[], d: T): T => (ok.includes(v as T) ? (v as T) : d)

export function loadSettings(store: Pick<Storage, 'getItem'> | null = safeStorage()): ReaderSettings {
  try {
    const raw = store?.getItem(KEY)
    if (!raw) return { ...DEFAULTS }
    const v = JSON.parse(raw) ?? {}
    return {
      paper: pick(v.paper, ['auto', 'light', 'dark', 'sepia'] as const, DEFAULTS.paper),
      fontSize: pick(v.fontSize, FONT_SIZES, DEFAULTS.fontSize),
      lineHeight: pick(v.lineHeight, LINE_HEIGHTS, DEFAULTS.lineHeight),
      margins: pick(v.margins, ['narrow', 'normal', 'wide'] as const, DEFAULTS.margins),
      flow: pick(v.flow, ['paginated', 'scrolled'] as const, DEFAULTS.flow),
      face: pick(v.face, ['newsreader', 'book'] as const, DEFAULTS.face),
    }
  } catch {
    return { ...DEFAULTS }
  }
}

export function saveSettings(s: ReaderSettings, store: Pick<Storage, 'setItem'> | null = safeStorage()): void {
  try {
    store?.setItem(KEY, JSON.stringify(s))
  } catch {
    // blocked: the choice lasts this page
  }
}

/** One step up or down a ladder of values (clamped). */
export function step<T>(ladder: readonly T[], cur: T, dir: 1 | -1): T {
  const i = Math.max(0, ladder.indexOf(cur))
  return ladder[Math.min(ladder.length - 1, Math.max(0, i + dir))]
}

/** The paginator's layout attributes for a margin choice. */
export function layoutFor(m: Margins, phone: boolean): { gap: string; margin: string; maxInline: string } {
  if (phone) return { gap: m === 'narrow' ? '4%' : m === 'wide' ? '12%' : '7%', margin: '56px', maxInline: '720px' }
  switch (m) {
    case 'narrow':
      return { gap: '4%', margin: '56px', maxInline: '860px' }
    case 'wide':
      return { gap: '10%', margin: '72px', maxInline: '600px' }
    default:
      return { gap: '7%', margin: '64px', maxInline: '720px' }
  }
}

export interface PaperColours {
  paper: string
  ink: string
  link: string
  mark: string
  scheme: 'light' | 'dark'
}

/** The CSS foliate puts in each section: the paper, the type, the rhythm.
 * Colours are passed in as values: a section's document is an iframe and
 * sees none of the app's custom properties. */
export function bookCss(s: ReaderSettings, c: PaperColours, fontUrls: { normal: string; italic: string }): string {
  const face =
    s.face === 'newsreader'
      ? `
    @font-face { font-family: 'Newsreader Variable'; font-style: normal; font-weight: 200 800; font-display: swap; src: url("${fontUrls.normal}") format('woff2-variations'); }
    @font-face { font-family: 'Newsreader Variable'; font-style: italic; font-weight: 200 800; font-display: swap; src: url("${fontUrls.italic}") format('woff2-variations'); }
    html, body, p, li, blockquote, dd, dt, div, span, td, th, h1, h2, h3, h4, h5, h6 { font-family: 'Newsreader Variable', Georgia, serif !important; }`
      : ''
  return `
    @namespace epub "http://www.idpf.org/2007/ops";
    ${face}
    html {
      color-scheme: ${c.scheme};
      background: ${c.paper} !important;
      color: ${c.ink} !important;
      font-size: ${s.fontSize}% !important;
      -webkit-text-size-adjust: none;
      text-size-adjust: none;
    }
    body { background: transparent !important; color: inherit !important; }
    /* the paper's ink wins over a book's own colours (1984's CSS sets p { color: #000 },
       black on the dark paper); links keep theirs below, images are untouched */
    body *:not(a) { color: inherit !important; background-color: transparent !important; }
    a:link, a:visited { color: ${c.link} !important; }
    p, li, blockquote, dd {
      line-height: ${s.lineHeight} !important;
      hyphens: auto;
      -webkit-hyphens: auto;
      widows: 2;
      orphans: 2;
    }
    [align="left"] { text-align: left; }
    [align="right"] { text-align: right; }
    [align="center"] { text-align: center; }
    pre { white-space: pre-wrap !important; }
    img, svg, video { max-width: 100%; height: auto; }
    ::selection { background: ${c.mark}; }
    aside[epub|type~="footnote"], aside[epub|type~="endnote"], aside[epub|type~="note"], aside[epub|type~="rearnote"] { display: none; }
  `
}

/** "12 min left in chapter" from foliate's estimate (minutes), or null when
 * there is none or it rounds to nothing. */
export function minutesLeft(m: number | undefined | null): string | null {
  if (m === undefined || m === null || !Number.isFinite(m)) return null
  const n = Math.round(m)
  if (n < 1) return m > 0 ? 'Less than a minute left in chapter' : null
  return `${n} min left in chapter`
}

/** The percent the progress bar shows. */
export function percentOf(fraction: number | undefined | null): number {
  if (fraction === undefined || fraction === null || !Number.isFinite(fraction)) return 0
  return Math.round(Math.max(0, Math.min(1, fraction)) * 100)
}
