// The app's in-book reader page: the web reader's engine (foliate-js at
// 78914ae, vendored in ui/vendor/foliate-js) and its XPointer converter
// (ui/src/reader/xpointer.ts), driven by native code through
// `window.taisce` and answering through `webkit.messageHandlers.reader`.
// Native owns the chrome, the settings and the sync; this page only
// renders, turns pages and reports where it is.
//
// Built by apple/scripts/build-reader.sh into
// apple/App/Taisce/Resources/reader/reader.js (one IIFE, no imports at
// runtime): the page loads nothing from the network.

import '../../ui/vendor/foliate-js/view.js'
import type { RelocateDetail, TocItem, View } from '../../ui/vendor/foliate-js/view.js'
import { parseXPointer, resolveXPointer, xpointerFromPoint, xpointerToRange } from '../../ui/src/reader/xpointer'

type Start = { kind: 'xpointer'; xpointer: string; fraction: number } | { kind: 'cfi'; cfi: string } | { kind: 'start' }

interface Look {
  paper: string
  ink: string
  link: string
  mark: string
  scheme: 'light' | 'dark'
  fontSize: number
  lineHeight: number
  gap: string
  margin: string
  maxInline: string
  flow: 'paginated' | 'scrolled'
  face: 'newsreader' | 'book'
  animated: boolean
}

declare global {
  interface Window {
    webkit?: { messageHandlers?: { reader?: { postMessage: (m: unknown) => void } } }
    taisce: unknown
  }
}

const post = (type: string, body: Record<string, unknown> = {}) => {
  try {
    window.webkit?.messageHandlers?.reader?.postMessage({ type, ...body })
  } catch {
    // no native side (a browser preview)
  }
}

let view: View | null = null
let look: Look | null = null

const titleOf = (t: unknown): string => {
  if (!t) return ''
  if (typeof t === 'string') return t
  if (typeof t === 'object') return String(Object.values(t as Record<string, string>)[0] ?? '')
  return ''
}

const authorOf = (a: unknown): string => {
  if (!a) return ''
  if (typeof a === 'string') return a
  if (Array.isArray(a)) return a.map(authorOf).filter(Boolean).join(', ')
  if (typeof a === 'object') return titleOf((a as { name?: unknown }).name)
  return ''
}

function flatToc(items: TocItem[] | null | undefined, depth = 0, out: { label: string; href: string; depth: number }[] = []) {
  for (const t of items ?? []) {
    out.push({ label: t.label?.trim() || 'Untitled', href: t.href, depth })
    if (t.subitems?.length) flatToc(t.subitems, depth + 1, out)
  }
  return out
}

/** Go to a KOReader XPointer, or its percentage when the pointer does not
 * resolve in this copy of the book (Reader.tsx goToXPointer). */
async function goToXPointer(xp: string, fraction: number): Promise<boolean> {
  const v = view
  if (!v) return false
  const x = parseXPointer(xp)
  const section = x ? v.book.sections[x.spine] : undefined
  if (x && section?.createDocument) {
    try {
      const probe = await section.createDocument()
      if (resolveXPointer(probe, x)) {
        await v.renderer.goTo({ index: x.spine, anchor: (doc) => xpointerToRange(doc, xp) ?? 0 })
        return true
      }
    } catch {
      // an unreadable section: the percentage below
    }
  }
  await v.goToFraction(Math.max(0, Math.min(1, fraction)))
  return false
}

function css(l: Look): string {
  const face =
    l.face === 'newsreader'
      ? `
    @font-face { font-family: 'Newsreader Taisce'; font-style: normal; font-weight: 200 800; src: url("taisce-reader://bundle/Newsreader-VF.ttf") format('truetype'); }
    @font-face { font-family: 'Newsreader Taisce'; font-style: italic; font-weight: 200 800; src: url("taisce-reader://bundle/Newsreader-Italic-VF.ttf") format('truetype'); }
    html, body, p, li, blockquote, dd, dt, div, span, td, th, h1, h2, h3, h4, h5, h6 { font-family: 'Newsreader Taisce', 'New York', Georgia, serif !important; }`
      : ''
  return `
    @namespace epub "http://www.idpf.org/2007/ops";
    ${face}
    html {
      color-scheme: ${l.scheme};
      background: ${l.paper} !important;
      color: ${l.ink} !important;
      font-size: ${l.fontSize}% !important;
      -webkit-text-size-adjust: none;
      text-size-adjust: none;
    }
    body { background: transparent !important; color: inherit !important; }
    a:link, a:visited { color: ${l.link} !important; }
    p, li, blockquote, dd {
      line-height: ${l.lineHeight} !important;
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
    ::selection { background: ${l.mark}; }
    aside[epub|type~="footnote"], aside[epub|type~="endnote"], aside[epub|type~="note"], aside[epub|type~="rearnote"] { display: none; }
  `
}

function applyLook() {
  const r = view?.renderer
  if (!r || !look) return
  document.documentElement.style.background = look.paper
  document.documentElement.style.colorScheme = look.scheme
  r.setAttribute('flow', look.flow)
  r.setAttribute('gap', look.gap)
  r.setAttribute('margin', look.margin)
  r.setAttribute('max-inline-size', look.maxInline)
  r.setAttribute('max-column-count', '2')
  if (look.animated) r.setAttribute('animated', '')
  else r.removeAttribute('animated')
  r.setStyles?.(css(look))
}

function onKey(e: KeyboardEvent) {
  const el = e.target as HTMLElement | null
  if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable)) return
  if (e.metaKey || e.ctrlKey || e.altKey) return
  const k = e.key
  if (k === 'Escape') {
    post('toggleChrome')
    return
  }
  let dir: 'left' | 'right' | 1 | -1 | null = null
  if (k === 'ArrowLeft') dir = 'left'
  else if (k === 'ArrowRight') dir = 'right'
  else if (k === 'ArrowDown' || k === 'PageDown' || k === 'j' || k === 'J' || (k === ' ' && !e.shiftKey)) dir = 1
  else if (k === 'ArrowUp' || k === 'PageUp' || k === 'k' || k === 'K' || (k === ' ' && e.shiftKey)) dir = -1
  if (dir === null) return
  e.preventDefault()
  turn(dir)
}

function turn(dir: 'left' | 'right' | 1 | -1) {
  const v = view
  if (!v) return
  post('moved')
  if (dir === 'left') void v.goLeft()
  else if (dir === 'right') void v.goRight()
  else if (dir === 1) void v.next()
  else void v.prev()
}

async function open(opts: { url: string; start: Start; look: Look }) {
  look = opts.look
  try {
    const res = await fetch(opts.url, { cache: 'no-store' })
    if (!res.ok) throw new Error(res.status === 404 ? 'This book is not on the server any more.' : `The book did not load (${res.status}).`)
    const blob = await res.blob()
    const v = document.createElement('foliate-view') as unknown as View
    view = v
    document.getElementById('host')!.append(v)
    v.addEventListener('load', (e: Event) => {
      const { doc } = (e as CustomEvent<{ doc: Document; index: number }>).detail
      doc.addEventListener('keydown', onKey)
      doc.addEventListener('wheel', () => post('moved'), { passive: true })
      doc.addEventListener('touchstart', () => post('moved'), { passive: true })
      doc.addEventListener('click', (ev) => {
        if (ev.defaultPrevented || (ev.target as Element | null)?.closest?.('a[href]')) return
        if (doc.getSelection()?.toString()) return
        const frame = doc.defaultView?.frameElement as HTMLElement | null
        const x = (frame?.getBoundingClientRect().left ?? 0) + ev.clientX
        const w = window.innerWidth
        const paged = v.renderer?.getAttribute('flow') !== 'scrolled'
        if (paged && x < w * 0.3) turn('left')
        else if (paged && x > w * 0.7) turn('right')
        else post('toggleChrome')
      })
    })
    v.addEventListener('relocate', (e: Event) => {
      const d = (e as CustomEvent<RelocateDetail>).detail
      const index = d.section?.current ?? 0
      const r = d.range
      let xp: string | null = null
      try {
        xp = r ? xpointerFromPoint(r.startContainer.ownerDocument ?? document, index, r.startContainer, r.startOffset) : null
      } catch {
        xp = null
      }
      post('relocate', {
        fraction: d.fraction,
        cfi: d.cfi,
        xpointer: xp ?? `/body/DocFragment[${index + 1}]/body`,
        chapter: d.tocItem?.label?.trim() ?? '',
        href: d.tocItem?.href ?? null,
        minutes: d.time?.section ?? null,
      })
    })
    await v.open(new File([blob], 'book.epub', { type: 'application/epub+zip' }))
    // a book's scripts never run (the page's CSP refuses them too)
    v.book.transformTarget?.addEventListener('load', (ev) => {
      const detail = (ev as CustomEvent<{ isScript: boolean; allow: boolean }>).detail
      if (detail.isScript) detail.allow = false
    })
    post('opened', {
      title: titleOf(v.book.metadata?.title),
      author: authorOf(v.book.metadata?.author),
      toc: flatToc(v.book.toc),
      rtl: v.book.dir === 'rtl',
    })
    applyLook()
    let exact = true
    if (opts.start.kind === 'xpointer') exact = await goToXPointer(opts.start.xpointer, opts.start.fraction)
    else if (opts.start.kind === 'cfi') await v.init({ lastLocation: opts.start.cfi })
    else await v.init({ showTextStart: false })
    post('ready', { exact })
  } catch (e) {
    post('error', { message: e instanceof Error ? e.message : String(e) })
  }
}

window.addEventListener('keydown', onKey)

// A tap in the page's margins (outside the section's iframe: wide screens
// have room either side of the column) turns the page too; taps inside the
// iframe are handled by its own listener and never reach here.
document.getElementById('host')?.addEventListener('click', (ev) => {
  const v = view
  if (!v || ev.defaultPrevented) return
  const w = window.innerWidth
  const paged = v.renderer?.getAttribute('flow') !== 'scrolled'
  if (paged && ev.clientX < w * 0.3) turn('left')
  else if (paged && ev.clientX > w * 0.7) turn('right')
  else post('toggleChrome')
})

window.taisce = {
  open,
  setLook(l: Look) {
    look = l
    applyLook()
  },
  next: () => turn(1),
  prev: () => turn(-1),
  left: () => turn('left'),
  right: () => turn('right'),
  async goTo(href: string) {
    post('moved')
    await view?.goTo(href)
  },
  async goToFraction(f: number) {
    post('moved')
    await view?.goToFraction(Math.max(0, Math.min(1, f)))
  },
  /** A jump to another device's place: not the reader's own move. */
  goToXPointer,
}

post('loaded')
