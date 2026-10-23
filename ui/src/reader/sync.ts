// The web reader as one more kosync device: where to open a book, when to
// say "Synced from …", and when to write the place back.
//
// The server keeps the kosync row the Kindle and the Xteink write (one per
// person and book, newest wins by the server's clock). The reader reads it
// on open and whenever the tab comes back into view, and writes its own
// place after a page turn (at most every 10 s), and on hide and close. A
// write carries `base`, the stamp of the row it last saw: if another device
// moved on since, the server refuses with that row (409), and the reader
// goes there instead of dragging the device back.

import { signalUnauthorized, withCsrf } from '../auth'

export type Source = 'book' | 'doc'

/** A kosync row as a device reads it: `percentage` the 0..1 fraction,
 * `timestamp` unix seconds. */
export interface Position {
  progress: string
  percentage: number
  device: string
  device_id: string
  timestamp: number
}

/** What this browser remembers per book (localStorage): foliate's own CFI is
 * finer than an XPointer, so it wins when the last place was ours. */
export interface LocalPlace {
  cfi: string
  fraction: number
  /** the server stamp this place was written (or seen) at; 0 if never */
  timestamp: number
}

/* ---------- this browser's device id ---------- */

const DEVICE_KEY = 'taisce.reader.device'

function randomId(): string {
  try {
    return crypto.randomUUID()
  } catch {
    return `web-${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`
  }
}

let sessionId: string | null = null

/** Stable per browser (localStorage); per tab when storage is off. */
export function deviceId(store: Pick<Storage, 'getItem' | 'setItem'> | null = safeStorage()): string {
  try {
    const got = store?.getItem(DEVICE_KEY)
    if (got) return got
    const id = randomId()
    store?.setItem(DEVICE_KEY, id)
    if (store) return id
  } catch {
    // private window, blocked storage: a per-tab id below
  }
  sessionId ??= randomId()
  return sessionId
}

export function safeStorage(): Storage | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage
  } catch {
    return null
  }
}

const placeKey = (source: Source, id: string) => `taisce.reader.place.${source}.${id}`

export function loadPlace(source: Source, id: string, store = safeStorage()): LocalPlace | null {
  try {
    const raw = store?.getItem(placeKey(source, id))
    if (!raw) return null
    const v = JSON.parse(raw)
    if (typeof v?.cfi !== 'string' || typeof v?.fraction !== 'number') return null
    return { cfi: v.cfi, fraction: v.fraction, timestamp: typeof v.timestamp === 'number' ? v.timestamp : 0 }
  } catch {
    return null
  }
}

export function savePlace(source: Source, id: string, place: LocalPlace, store = safeStorage()): void {
  try {
    store?.setItem(placeKey(source, id), JSON.stringify(place))
  } catch {
    // storage full or off: the server copy is the record anyway
  }
}

/* ---------- decisions (pure) ---------- */

export type Start =
  | { kind: 'xpointer'; xpointer: string; fraction: number; from: Position }
  | { kind: 'cfi'; cfi: string; fraction: number }
  | { kind: 'fraction'; fraction: number }
  | { kind: 'start' }

/** Where to open: another device's newer place (by its XPointer, the
 * percentage behind it), else this browser's own CFI, else our own server
 * row (another browser of ours, or local storage was cleared), else the
 * beginning. */
export function startTarget(remote: Position | null, local: LocalPlace | null, mine: string): Start {
  if (remote && (remote.device_id !== mine || !local) && remote.timestamp > (local?.timestamp ?? 0)) {
    return { kind: 'xpointer', xpointer: remote.progress, fraction: remote.percentage, from: remote }
  }
  if (local) return { kind: 'cfi', cfi: local.cfi, fraction: local.fraction }
  if (remote) return { kind: 'xpointer', xpointer: remote.progress, fraction: remote.percentage, from: remote }
  return { kind: 'start' }
}

/** The quiet toast after a jump: only for another device's place. */
export function syncToast(p: Position, mine: string): string | null {
  if (p.device_id === mine) return null
  const pct = Math.round(Math.max(0, Math.min(1, p.percentage)) * 100)
  return `Synced from ${p.device || 'another device'} · ${pct}%`
}

/** Has another device moved on since `seen` (the stamp we last saw)? */
export function isNewer(remote: Position | null, seen: number, mine: string): remote is Position {
  return !!remote && remote.device_id !== mine && remote.timestamp > seen
}

/* ---------- the routes ---------- */

const base = (source: Source, id: string) => `/api/${source === 'book' ? 'books' : 'docs'}/${encodeURIComponent(id)}`

/** The EPUB's URL: a library book streams through the daemon; a doc renders. */
export function fileUrl(source: Source, id: string): string {
  return source === 'book' ? `${base('book', id)}/file` : `${base('doc', id)}/epub`
}

export type GetResult = { ok: true; position: Position | null } | { ok: false; unavailable: true }

/** The stored place; `unavailable` when this server keeps none for us
 * (LOCAL mode, an older server: 404). */
export async function getPosition(source: Source, id: string, f: typeof fetch = fetch): Promise<GetResult> {
  const r = await f(`${base(source, id)}/position`, { cache: 'no-store' })
  if (r.status === 401) signalUnauthorized()
  if (!r.ok) return { ok: false, unavailable: true }
  const j = await r.json()
  return { ok: true, position: j?.position ?? null }
}

export type PutResult = { kind: 'written'; position: Position } | { kind: 'newer'; position: Position } | { kind: 'failed' }

export async function putPosition(
  source: Source,
  id: string,
  body: { progress: string; percentage: number; device_id: string; base: number },
  opts: { keepalive?: boolean } = {},
  f: typeof fetch = fetch,
): Promise<PutResult> {
  try {
    const r = await f(
      `${base(source, id)}/position`,
      withCsrf({ method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body), keepalive: opts.keepalive }),
    )
    const j = await r.json().catch(() => null)
    if (r.status === 409 && j?.position) return { kind: 'newer', position: j.position }
    if (r.ok && j?.position) return { kind: 'written', position: j.position }
    return { kind: 'failed' }
  } catch {
    return { kind: 'failed' }
  }
}

/* ---------- the writer ---------- */

export interface Here {
  progress: string
  percentage: number
}

export interface WriterDeps {
  put: (body: { progress: string; percentage: number; device_id: string; base: number }, keepalive: boolean) => Promise<PutResult>
  /** another device got there first: go there */
  onNewer: (p: Position) => void
  /** each successful write (the stamp to remember) */
  onWritten?: (p: Position) => void
  now?: () => number
  setTimer?: (fn: () => void, ms: number) => unknown
  clearTimer?: (t: unknown) => void
  /** at most one write per this many ms while reading */
  minIntervalMs?: number
}

/** Debounced, guarded writes of the reader's place. `moved` after each page
 * turn; `flush(true)` on hide and close (keepalive survives the unload). */
export class PositionWriter {
  #deps: Required<Omit<WriterDeps, 'onWritten'>> & Pick<WriterDeps, 'onWritten'>
  #device: string
  #seen: number
  #pending: Here | null = null
  #written: string | null = null
  #lastWrite = -Infinity
  #timer: unknown = null
  #inFlight: Promise<void> | null = null
  #stopped = false

  constructor(device: string, seen: number, deps: WriterDeps) {
    this.#device = device
    this.#seen = seen
    this.#deps = {
      now: () => Date.now(),
      setTimer: (fn, ms) => setTimeout(fn, ms),
      clearTimer: (t) => clearTimeout(t as ReturnType<typeof setTimeout>),
      minIntervalMs: 10_000,
      ...deps,
    }
  }

  /** The stamp of the newest row the reader has seen (a fetch on return). */
  seen(stamp: number): void {
    this.#seen = Math.max(this.#seen, stamp)
  }

  get pending(): boolean {
    return this.#pending !== null
  }

  /** The reader moved (a page turn, a jump, a scroll). */
  moved(here: Here): void {
    if (this.#stopped) return
    if (here.progress === this.#written && !this.#pending) return
    this.#pending = here
    if (this.#timer !== null) return
    const wait = Math.max(0, this.#lastWrite + this.#deps.minIntervalMs - this.#deps.now())
    this.#timer = this.#deps.setTimer(() => {
      this.#timer = null
      void this.flush(false)
    }, wait)
  }

  /** Write the pending place now (if any). */
  async flush(keepalive: boolean): Promise<void> {
    if (this.#timer !== null) {
      this.#deps.clearTimer(this.#timer)
      this.#timer = null
    }
    if (this.#inFlight) await this.#inFlight
    const here = this.#pending
    if (!here || this.#stopped) return
    this.#pending = null
    if (here.progress === this.#written) return
    this.#lastWrite = this.#deps.now()
    this.#inFlight = (async () => {
      const r = await this.#deps.put({ ...here, device_id: this.#device, base: this.#seen }, keepalive)
      if (r.kind === 'written') {
        this.#written = here.progress
        this.#seen = Math.max(this.#seen, r.position.timestamp)
        this.#deps.onWritten?.(r.position)
      } else if (r.kind === 'newer') {
        this.#seen = Math.max(this.#seen, r.position.timestamp)
        this.#deps.onNewer(r.position)
      } else if (!this.#pending) {
        // a failed write is retried with the next move (or the next flush)
        this.#pending = here
      }
    })()
    await this.#inFlight
    this.#inFlight = null
  }

  /** Forget the unwritten place (another device's newer one is being taken). */
  discard(): void {
    this.#pending = null
    if (this.#timer !== null) this.#deps.clearTimer(this.#timer)
    this.#timer = null
  }

  stop(): void {
    this.#stopped = true
    if (this.#timer !== null) this.#deps.clearTimer(this.#timer)
    this.#timer = null
  }
}
