// The web reader: a book (or a doc's EPUB) in the whole window, on
// foliate-js (ui/vendor/foliate-js), as one more kosync device.
//
// - Opens where you left off: another device's newer place (KOReader's
//   XPointer, else its percentage) with a quiet "Synced from …" toast, else
//   this browser's own place, else the start (sync.ts startTarget).
// - Writes its place back after page turns (at most every 10 s), on hide
//   and on close; a newer place from another device is taken instead of
//   overwritten (sync.ts PositionWriter, the server's 409).
// - The bar (Back, title, chapter, contents, search, settings) hides while
//   you read and comes back with the pointer at the top, a tap in the
//   middle, or Esc. Keys: ← → Space PgUp PgDn J K. Taps on the left and
//   right thirds turn pages; swipes too (foliate's paginator).

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import '../../vendor/foliate-js/view.js'
import type { RelocateDetail, SearchExcerpt, TocItem, View } from '../../vendor/foliate-js/view.js'
import normalFont from '@fontsource-variable/newsreader/files/newsreader-latin-opsz-normal.woff2?url'
import italicFont from '@fontsource-variable/newsreader/files/newsreader-latin-opsz-italic.woff2?url'
import { Button, Icon, IconButton, Spinner, useTheme } from '../lapis'
import { Notices, notify } from '../Notice'
import { navigate } from '../router'
import { parseXPointer, resolveXPointer, xpointerFromPoint, xpointerToRange } from './xpointer'
import {
  deviceId,
  fileUrl,
  getPosition,
  isNewer,
  loadPlace,
  type Position,
  PositionWriter,
  putPosition,
  savePlace,
  type Source,
  startTarget,
  syncToast,
} from './sync'
import {
  bookCss,
  FONT_SIZES,
  LINE_HEIGHTS,
  layoutFor,
  loadSettings,
  minutesLeft,
  percentOf,
  type ReaderSettings,
  saveSettings,
  step,
} from './settings'
import './reader.css'

type Panel = null | 'toc' | 'settings' | 'search'

const PHONE = '(max-width: 760px)'
const isPhone = () => {
  try {
    return matchMedia(PHONE).matches
  } catch {
    return false
  }
}

function titleOf(t: unknown): string {
  if (!t) return ''
  if (typeof t === 'string') return t
  if (typeof t === 'object') return String(Object.values(t as Record<string, string>)[0] ?? '')
  return ''
}

/** Go to a KOReader XPointer, or to its percentage when the pointer does not
 * resolve in this copy of the book. True when the pointer was exact. */
export async function goToXPointer(view: View, xp: string, fraction: number): Promise<boolean> {
  const x = parseXPointer(xp)
  const section = x ? view.book.sections[x.spine] : undefined
  if (x && section?.createDocument) {
    try {
      const probe = await section.createDocument()
      if (resolveXPointer(probe, x)) {
        await view.renderer.goTo({ index: x.spine, anchor: (doc) => xpointerToRange(doc, xp) ?? 0 })
        return true
      }
    } catch {
      // an unreadable section: the percentage below
    }
  }
  await view.goToFraction(Math.max(0, Math.min(1, fraction)))
  return false
}

export default function Reader({ source, id }: { source: Source; id: string }) {
  const { theme } = useTheme()
  const [settings, setSettingsRaw] = useState<ReaderSettings>(loadSettings)
  const setSettings = useCallback((patch: Partial<ReaderSettings>) => {
    setSettingsRaw((s) => {
      const next = { ...s, ...patch }
      saveSettings(next)
      return next
    })
  }, [])
  const paper = settings.paper === 'auto' ? theme : settings.paper
  const settingsRef = useRef(settings)
  settingsRef.current = settings

  const rootRef = useRef<HTMLDivElement>(null)
  const hostRef = useRef<HTMLDivElement>(null)
  const viewRef = useRef<View | null>(null)
  const writerRef = useRef<PositionWriter | null>(null)
  const seenRef = useRef(0)
  // writes only follow the reader's own moves, never a jump we made
  const armedRef = useRef(false)
  const mine = useMemo(() => deviceId(), [])

  const [status, setStatus] = useState<'loading' | 'ready' | 'error'>('loading')
  const [error, setError] = useState('')
  const [title, setTitle] = useState('')
  const [toc, setToc] = useState<TocItem[]>([])
  const [loc, setLoc] = useState<{ fraction: number; chapter: string; minutes: number | null; href: string | null }>({
    fraction: 0,
    chapter: '',
    minutes: null,
    href: null,
  })
  const [chrome, setChrome] = useState(true)
  const [panel, setPanel] = useState<Panel>(null)
  const panelRef = useRef<Panel>(null)
  panelRef.current = panel

  useEffect(() => {
    if (title) document.title = `${title} · Taisce`
  }, [title])

  const back = useCallback(() => {
    if (source === 'book') navigate({ kind: 'books' })
    else navigate({ kind: 'doc', id })
  }, [source, id])

  /* ---------- moving ---------- */

  const arm = () => {
    armedRef.current = true
  }
  const turn = useCallback((dir: 1 | -1 | 'left' | 'right') => {
    const v = viewRef.current
    if (!v) return
    arm()
    setChrome(false)
    if (dir === 'left') void v.goLeft()
    else if (dir === 'right') void v.goRight()
    else if (dir === 1) void v.next()
    else void v.prev()
  }, [])

  /** A jump to another device's place, and the toast that says so. */
  const takeRemote = useCallback(async (p: Position) => {
    const v = viewRef.current
    if (!v) return
    armedRef.current = false
    writerRef.current?.seen(p.timestamp)
    seenRef.current = Math.max(seenRef.current, p.timestamp)
    await goToXPointer(v, p.progress, p.percentage)
    const t = syncToast(p, mine)
    if (t) notify(t, 'ok', { ttlMs: 4000 })
  }, [mine])

  /* ---------- keys ---------- */

  const onKey = useCallback(
    (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null
      if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable)) return
      if (e.metaKey || e.ctrlKey || e.altKey) return
      const k = e.key
      if (k === 'Escape') {
        if (panelRef.current) setPanel(null)
        else setChrome((c) => !c)
        return
      }
      if (panelRef.current) return
      let dir: 1 | -1 | 'left' | 'right' | null = null
      if (k === 'ArrowLeft') dir = 'left'
      else if (k === 'ArrowRight') dir = 'right'
      else if (k === 'PageDown' || k === 'j' || k === 'J' || (k === ' ' && !e.shiftKey)) dir = 1
      else if (k === 'PageUp' || k === 'k' || k === 'K' || (k === ' ' && e.shiftKey)) dir = -1
      if (dir === null) return
      e.preventDefault()
      turn(dir)
    },
    [turn],
  )
  useEffect(() => {
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onKey])
  const onKeyRef = useRef(onKey)
  onKeyRef.current = onKey

  /* ---------- open the book ---------- */

  useEffect(() => {
    let dead = false
    const view = document.createElement('foliate-view') as unknown as View
    view.className = 'rd-view'
    viewRef.current = view
    hostRef.current?.append(view)

    const onLoad = (e: Event) => {
      const { doc } = (e as CustomEvent<{ doc: Document; index: number }>).detail
      doc.addEventListener('keydown', (ev) => onKeyRef.current(ev))
      doc.addEventListener('wheel', arm, { passive: true })
      doc.addEventListener('touchstart', arm, { passive: true })
      doc.addEventListener('click', (ev) => {
        if (ev.defaultPrevented || (ev.target as Element | null)?.closest?.('a[href]')) return
        if (doc.getSelection()?.toString()) return
        if (panelRef.current) {
          setPanel(null)
          return
        }
        const frame = doc.defaultView?.frameElement as HTMLElement | null
        const x = (frame?.getBoundingClientRect().left ?? 0) + ev.clientX
        const w = window.innerWidth
        const paged = view.renderer?.getAttribute('flow') !== 'scrolled'
        if (paged && x < w * 0.3) turn('left')
        else if (paged && x > w * 0.7) turn('right')
        else setChrome((c) => !c)
      })
    }
    const onRelocate = (e: Event) => {
      const d = (e as CustomEvent<RelocateDetail>).detail
      setLoc({ fraction: d.fraction, chapter: d.tocItem?.label?.trim() ?? '', minutes: d.time?.section ?? null, href: d.tocItem?.href ?? null })
      savePlace(source, id, { cfi: d.cfi, fraction: d.fraction, timestamp: seenRef.current })
      if (!armedRef.current || !writerRef.current) return
      const index = d.section?.current ?? 0
      const r = d.range
      const xp =
        (r && xpointerFromPoint(r.startContainer.ownerDocument ?? document, index, r.startContainer, r.startOffset)) ||
        `/body/DocFragment[${index + 1}]/body`
      writerRef.current.moved({ progress: xp, percentage: Math.max(0, Math.min(1, d.fraction)) })
    }
    view.addEventListener('load', onLoad)
    view.addEventListener('relocate', onRelocate)

    ;(async () => {
      try {
        const res = await fetch(fileUrl(source, id), { cache: 'no-store' })
        if (!res.ok) throw new Error(res.status === 404 ? 'This book is not here.' : `The book did not load (${res.status}).`)
        const blob = await res.blob()
        const [remote] = await Promise.all([getPosition(source, id).catch(() => ({ ok: false as const, unavailable: true as const }))])
        if (dead) return
        await view.open(new File([blob], 'book.epub', { type: 'application/epub+zip' }))
        if (dead) return
        // a book's scripts never run (the CSP refuses them too)
        view.book.transformTarget?.addEventListener('load', (ev) => {
          const detail = (ev as CustomEvent<{ isScript: boolean; allow: boolean }>).detail
          if (detail.isScript) detail.allow = false
        })
        setTitle(titleOf(view.book.metadata?.title) || 'Untitled')
        setToc(view.book.toc ?? [])
        applyLook(view, settingsRef.current, rootRef.current)

        const position = remote.ok ? remote.position : null
        const local = loadPlace(source, id)
        seenRef.current = Math.max(position?.timestamp ?? 0, local?.timestamp ?? 0)
        const start = startTarget(position, local, mine)
        if (start.kind === 'xpointer') {
          await goToXPointer(view, start.xpointer, start.fraction)
          const t = syncToast(start.from, mine)
          if (t) notify(t, 'ok', { ttlMs: 4000 })
        } else if (start.kind === 'cfi') {
          await view.init({ lastLocation: start.cfi })
        } else {
          await view.init({ showTextStart: false })
        }
        if (dead) return
        if (remote.ok) {
          writerRef.current = new PositionWriter(mine, seenRef.current, {
            put: (b, keepalive) => putPosition(source, id, b, { keepalive }),
            onNewer: (p) => void takeRemote(p),
            onWritten: (p) => {
              seenRef.current = Math.max(seenRef.current, p.timestamp)
            },
          })
        }
        setStatus('ready')
      } catch (e) {
        if (dead) return
        setError(e instanceof Error ? e.message : String(e))
        setStatus('error')
      }
    })()

    const onVisible = async () => {
      const w = writerRef.current
      if (document.visibilityState === 'hidden') {
        void w?.flush(true)
        return
      }
      if (!w) return
      const r = await getPosition(source, id).catch(() => null)
      if (r?.ok && isNewer(r.position, seenRef.current, mine)) {
        w.discard()
        await takeRemote(r.position)
      }
    }
    const onHide = () => void writerRef.current?.flush(true)
    document.addEventListener('visibilitychange', onVisible)
    window.addEventListener('pagehide', onHide)
    return () => {
      dead = true
      document.removeEventListener('visibilitychange', onVisible)
      window.removeEventListener('pagehide', onHide)
      const w = writerRef.current
      writerRef.current = null
      if (w) void w.flush(true).finally(() => w.stop())
      try {
        view.close()
      } catch {
        // half-open: nothing to close
      }
      view.remove()
      viewRef.current = null
    }
  }, [source, id, mine, takeRemote, turn])

  /* ---------- the look ---------- */

  useEffect(() => {
    const v = viewRef.current
    if (v?.renderer && status === 'ready') applyLook(v, settings, rootRef.current)
  }, [settings, paper, status])

  // the bar hides on its own a moment after the book opens
  useEffect(() => {
    if (status !== 'ready') return
    const t = setTimeout(() => setChrome(false), 2500)
    return () => clearTimeout(t)
  }, [status])
  const showChrome = chrome || panel !== null || status !== 'ready'

  const pct = percentOf(loc.fraction)
  const left = minutesLeft(loc.minutes)

  return (
    <div
      ref={rootRef}
      className={`rd${showChrome ? ' rd-chrome' : ''}`}
      data-paper={paper}
      onMouseMove={(e) => {
        if (e.clientY < 72 && !isPhone()) setChrome(true)
      }}
    >
      <Notices />
      <header className="rd-bar" aria-hidden={!showChrome}>
        <Button variant="ghost" size="sm" icon="back" onClick={back} className="rd-back">
          <span className="rd-back-label">{source === 'book' ? 'Books' : 'Back to doc'}</span>
        </Button>
        <div className="rd-titles">
          <span className="rd-title">{title || ' '}</span>
          {loc.chapter && <span className="rd-chapter">{loc.chapter}</span>}
        </div>
        <div className="rd-tools">
          <IconButton icon="menu" label="Contents" active={panel === 'toc'} onClick={() => setPanel((p) => (p === 'toc' ? null : 'toc'))} disabled={status !== 'ready'} />
          <IconButton icon="search" label="Search in book" active={panel === 'search'} onClick={() => setPanel((p) => (p === 'search' ? null : 'search'))} disabled={status !== 'ready'} />
          <IconButton icon="settings" label="Reading settings" active={panel === 'settings'} onClick={() => setPanel((p) => (p === 'settings' ? null : 'settings'))} />
        </div>
      </header>

      <div className="rd-stage" ref={hostRef} />

      {status === 'loading' && (
        <div className="rd-state">
          <Spinner />
          <span>Opening the book…</span>
        </div>
      )}
      {status === 'error' && (
        <div className="rd-state">
          <Icon name="warning" size={22} />
          <span>{error}</span>
          <Button size="sm" onClick={back}>
            {source === 'book' ? 'Back to Books' : 'Back to the doc'}
          </Button>
        </div>
      )}

      <footer className="rd-foot">
        <div className="rd-foot-line">
          <span className="rd-pct">{status === 'ready' ? `${pct}%` : ''}</span>
          <span className="rd-left">{status === 'ready' ? (left ?? '') : ''}</span>
        </div>
        <div className="rd-progress" role="progressbar" aria-label="Through the book" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
          <span style={{ width: `${pct}%` }} />
        </div>
      </footer>

      {panel === 'toc' && (
        <Drawer title="Contents" onClose={() => setPanel(null)}>
          {toc.length === 0 ? (
            <p className="rd-muted">This book has no table of contents.</p>
          ) : (
            <TocList
              items={toc}
              current={loc.href}
              onGo={(href) => {
                arm()
                setPanel(null)
                void viewRef.current?.goTo(href)
              }}
            />
          )}
        </Drawer>
      )}
      {panel === 'search' && (
        <SearchPanel
          view={viewRef.current}
          onClose={() => setPanel(null)}
          onGo={(cfi) => {
            arm()
            void viewRef.current?.goTo(cfi)
            if (isPhone()) setPanel(null)
          }}
        />
      )}
      {panel === 'settings' && <SettingsPopover settings={settings} onChange={setSettings} onClose={() => setPanel(null)} />}
    </div>
  )
}

/** The paginator's attributes and the section CSS for these settings, with
 * the paper's colours read off the reader's root (the iframe sees no
 * custom properties). */
function applyLook(view: View, s: ReaderSettings, root: HTMLElement | null) {
  const r = view.renderer
  if (!r) return
  const l = layoutFor(s.margins, isPhone())
  r.setAttribute('flow', s.flow)
  r.setAttribute('gap', l.gap)
  r.setAttribute('margin', l.margin)
  r.setAttribute('max-inline-size', l.maxInline)
  r.setAttribute('max-column-count', '2')
  const cs = root ? getComputedStyle(root) : null
  const v = (name: string, d: string) => cs?.getPropertyValue(name).trim() || d
  const scheme = root?.dataset.paper === 'dark' ? 'dark' : 'light'
  const abs = (u: string) => new URL(u, location.href).href
  r.setStyles?.(
    bookCss(
      s,
      { paper: v('--read-paper', '#ffffff'), ink: v('--read-ink', '#1b1f29'), link: v('--read-link', '#3346d3'), mark: v('--read-select', 'rgba(51,70,211,.2)'), scheme },
      { normal: abs(normalFont), italic: abs(italicFont) },
    ),
  )
}

/* ---------- panels ---------- */

function Drawer({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  return (
    <div className="rd-scrim" onMouseDown={onClose}>
      <aside className="rd-drawer" aria-label={title} onMouseDown={(e) => e.stopPropagation()}>
        <div className="rd-drawer-head">
          <h2>{title}</h2>
          <IconButton icon="close" label="Close" onClick={onClose} />
        </div>
        <div className="rd-drawer-body">{children}</div>
      </aside>
    </div>
  )
}

function TocList({ items, current, onGo, depth = 0 }: { items: TocItem[]; current: string | null; onGo: (href: string) => void; depth?: number }) {
  return (
    <ul className={depth ? 'rd-toc rd-toc-sub' : 'rd-toc'}>
      {items.map((t, i) => (
        <li key={`${t.href}-${i}`}>
          <button type="button" className={`rd-toc-row${t.href === current ? ' on' : ''}`} aria-current={t.href === current ? 'location' : undefined} onClick={() => onGo(t.href)}>
            {t.label?.trim() || 'Untitled'}
          </button>
          {t.subitems && t.subitems.length > 0 && <TocList items={t.subitems} current={current} onGo={onGo} depth={depth + 1} />}
        </li>
      ))}
    </ul>
  )
}

interface Hit {
  cfi: string
  excerpt: SearchExcerpt
  label: string
}

function SearchPanel({ view, onClose, onGo }: { view: View | null; onClose: () => void; onGo: (cfi: string) => void }) {
  const [q, setQ] = useState('')
  const [hits, setHits] = useState<Hit[] | null>(null)
  const [busy, setBusy] = useState<number | null>(null)
  const run = useRef(0)
  useEffect(() => () => view?.clearSearch(), [view])
  const search = async () => {
    if (!view || !q.trim()) return
    const me = ++run.current
    const out: Hit[] = []
    setHits([])
    setBusy(0)
    try {
      for await (const r of view.search({ query: q.trim() })) {
        if (me !== run.current) return
        if (r === 'done') break
        if ('progress' in r) setBusy(Math.round(r.progress * 100))
        else if ('subitems' in r) {
          for (const s of r.subitems) out.push({ cfi: s.cfi, excerpt: s.excerpt, label: r.label })
          setHits([...out])
        }
      }
    } finally {
      if (me === run.current) setBusy(null)
    }
  }
  return (
    <Drawer title="Search in book" onClose={onClose}>
      <form
        className="rd-search"
        onSubmit={(e) => {
          e.preventDefault()
          void search()
        }}
      >
        <input autoFocus type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder="Find a word or phrase" aria-label="Find in this book" />
        <Button type="submit" size="sm" variant="primary" disabled={!q.trim()}>
          Find
        </Button>
      </form>
      {busy !== null && <p className="rd-muted">Searching… {busy}%</p>}
      {hits && hits.length === 0 && busy === null && <p className="rd-muted">Nothing in this book matches “{q.trim()}”.</p>}
      {hits && hits.length > 0 && (
        <ul className="rd-hits">
          {hits.map((h, i) => (
            <li key={`${h.cfi}-${i}`}>
              <button type="button" className="rd-hit" onClick={() => onGo(h.cfi)}>
                {h.label && <span className="rd-hit-where">{h.label}</span>}
                <span className="rd-hit-text">
                  {h.excerpt.pre}
                  <mark>{h.excerpt.match}</mark>
                  {h.excerpt.post}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </Drawer>
  )
}

function Choice<T extends string | number>({ label, value, options, onChange }: { label: string; value: T; options: { id: T; label: string }[]; onChange: (v: T) => void }) {
  return (
    <div className="rd-set">
      <span className="rd-set-label">{label}</span>
      <div className="rd-seg" role="radiogroup" aria-label={label}>
        {options.map((o) => (
          <button key={String(o.id)} type="button" role="radio" aria-checked={o.id === value} className={o.id === value ? 'on' : ''} onClick={() => onChange(o.id)}>
            {o.label}
          </button>
        ))}
      </div>
    </div>
  )
}

function Stepper({ label, value, onStep, atMin, atMax }: { label: string; value: string; onStep: (d: 1 | -1) => void; atMin: boolean; atMax: boolean }) {
  return (
    <div className="rd-set">
      <span className="rd-set-label">{label}</span>
      <div className="rd-step">
        <button type="button" aria-label={`Smaller ${label.toLowerCase()}`} disabled={atMin} onClick={() => onStep(-1)}>
          −
        </button>
        <span aria-live="polite">{value}</span>
        <button type="button" aria-label={`Larger ${label.toLowerCase()}`} disabled={atMax} onClick={() => onStep(1)}>
          +
        </button>
      </div>
    </div>
  )
}

function SettingsPopover({ settings: s, onChange, onClose }: { settings: ReaderSettings; onChange: (p: Partial<ReaderSettings>) => void; onClose: () => void }) {
  return (
    <div className="rd-pop-scrim" onMouseDown={onClose}>
      <div className="rd-pop" role="dialog" aria-label="Reading settings" onMouseDown={(e) => e.stopPropagation()}>
        <Choice
          label="Theme"
          value={s.paper}
          onChange={(paper) => onChange({ paper })}
          options={[
            { id: 'auto', label: 'Auto' },
            { id: 'light', label: 'Light' },
            { id: 'sepia', label: 'Sepia' },
            { id: 'dark', label: 'Dark' },
          ]}
        />
        <Stepper
          label="Text size"
          value={`${s.fontSize}%`}
          atMin={s.fontSize === FONT_SIZES[0]}
          atMax={s.fontSize === FONT_SIZES[FONT_SIZES.length - 1]}
          onStep={(d) => onChange({ fontSize: step(FONT_SIZES, s.fontSize, d) })}
        />
        <Stepper
          label="Line height"
          value={s.lineHeight.toFixed(2).replace(/0$/, '')}
          atMin={s.lineHeight === LINE_HEIGHTS[0]}
          atMax={s.lineHeight === LINE_HEIGHTS[LINE_HEIGHTS.length - 1]}
          onStep={(d) => onChange({ lineHeight: step(LINE_HEIGHTS, s.lineHeight, d) })}
        />
        <Choice
          label="Margins"
          value={s.margins}
          onChange={(margins) => onChange({ margins })}
          options={[
            { id: 'narrow', label: 'Narrow' },
            { id: 'normal', label: 'Normal' },
            { id: 'wide', label: 'Wide' },
          ]}
        />
        <Choice
          label="Layout"
          value={s.flow}
          onChange={(flow) => onChange({ flow })}
          options={[
            { id: 'paginated', label: 'Pages' },
            { id: 'scrolled', label: 'Scroll' },
          ]}
        />
        <Choice
          label="Typeface"
          value={s.face}
          onChange={(face) => onChange({ face })}
          options={[
            { id: 'newsreader', label: 'Newsreader' },
            { id: 'book', label: 'The book’s' },
          ]}
        />
      </div>
    </div>
  )
}
