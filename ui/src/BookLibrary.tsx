// /books, the EPUB library (canvas Books, SERVER mode only): the title and
// a filter; Currently reading cards with progress; the dropzone with one
// result pill per file; the Library cover grid (All / Unread / Finished,
// newest first, "Load more" pages through next_before); and, beside it, the
// selected book's details (edit title/author/series, delete) over an
// E-readers card. On a narrow window the details open in a sheet.
//
// A file dropped anywhere on the page uploads (or is swallowed): it never
// navigates the browser away. Covers come from /api/books/{id}/cover; a
// book without one wears the typographic cover (bookCovers.ts).
//
// Above them all, "From Taisce": the docs this person sent to their
// e-readers (ereader.ts), each a Download and a Stop sending.
//
// Colour covers (ColourCovers.tsx): "Find colour covers" in the head while a
// loaded book has a black-and-white cover (or one not looked at yet) opens
// the library pass; the details carry "Find a colour cover…".

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore, type CSSProperties } from 'react'
import { errText, notify } from './Notice'
import { relTime } from './time'
import BookUpload from './BookUpload'
import { bookUploads, useBookUploads } from './bookUploads'
import { coverArt } from './bookCovers'
import Link from './Link'
import { navigate } from './router'
import { DeviceIcon } from './screens/DeviceIcon'
import { Button, buttonClass, Card, ConfirmButton, EmptyState, Icon, Sheet, Skeleton } from './lapis'
import {
  Book,
  BookPatch,
  BookProgress,
  Ereader,
  ReadingBook,
  activeEreaders,
  coverUrl,
  currentlyReading,
  deleteBook,
  fetchProgress,
  formatSize,
  listBooks,
  listEreaders,
  parseSeriesIndex,
  searchFilter,
  showColourPass,
  updateBook,
} from './books'
import { chaptersLabel, downloadEpub, listSentBooks, setSending, type SentBook } from './ereader'
import { ColourCoverPanel, CoverReviewSheet } from './ColourCovers'
import './screens/books.css'

const PAGE = 60

export type Shelf = 'all' | 'unread' | 'finished'

/** The Library's All / Unread / Finished switch, over the loaded books. */
export function shelfFilter(books: Book[], progress: Record<string, BookProgress | null>, shelf: Shelf): Book[] {
  if (shelf === 'all') return books
  return books.filter((b) => {
    const s = progress[b.id]?.status ?? 'unread'
    return shelf === 'finished' ? s === 'finished' : s === 'unread'
  })
}

/** The corner badge on a cover: Finished, New (added in the last day), or none. */
export function coverBadge(book: Pick<Book, 'added_at'>, p: BookProgress | null, now = Date.now()): string | null {
  if (p?.status === 'finished') return 'Finished'
  const t = Date.parse(book.added_at)
  if (Number.isFinite(t) && now - t < 86_400_000) return 'New'
  return null
}

/** 0–100 for the bar; finished is 100. */
export function pctOf(p: Pick<BookProgress, 'percentage' | 'status'> | null | undefined): number {
  if (!p) return 0
  if (p.status === 'finished') return 100
  return Math.max(0, Math.min(100, Math.round(p.percentage)))
}

/** Where the reading is: "Kindle · 2h ago", "finished · 3d ago", "not started". */
export function progressWhere(p: BookProgress | null): string {
  if (!p || p.status === 'unread') return 'not started'
  if (p.status === 'finished') return `finished ${relTime(p.updated_at)}`
  return `${p.device} · ${relTime(p.updated_at)}`
}

/** The lead line under the title. */
export function booksLead(n: { books: number; more: boolean; reading: number; finished: number; ereaders: number | null }): string {
  const parts = [`${n.books}${n.more ? '+' : ''} book${n.books === 1 && !n.more ? '' : 's'}`, `${n.reading} reading`, `${n.finished} finished`]
  if (n.ereaders !== null) parts.push(n.ereaders === 0 ? 'no e-readers yet' : `synced to ${n.ereaders} e-reader${n.ereaders === 1 ? '' : 's'}`)
  return parts.join(' · ')
}

// the details sit beside the grid only when there is room for both columns
const WIDE_QUERY = '(min-width: 1280px)'
function subscribeWide(fn: () => void) {
  if (typeof matchMedia !== 'function') return () => {}
  const m = matchMedia(WIDE_QUERY)
  m.addEventListener('change', fn)
  return () => m.removeEventListener('change', fn)
}
const isWideNow = () => typeof matchMedia === 'function' && matchMedia(WIDE_QUERY).matches

export default function Books({ dataVersion }: { dataVersion: number }) {
  const [books, setBooks] = useState<Book[]>([])
  const [nextBefore, setNextBefore] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [shelf, setShelf] = useState<Shelf>('all')
  const [loading, setLoading] = useState(false)
  const [loaded, setLoaded] = useState(false)
  const [selected, setSelected] = useState<string | null>(null)
  const [progress, setProgress] = useState<Record<string, BookProgress | null>>({})
  const [reading, setReading] = useState<ReadingBook[] | null>(null)
  const [ereaders, setEreaders] = useState<Ereader[] | null>(null)
  const [reviewing, setReviewing] = useState(false)
  const wide = useSyncExternalStore(subscribeWide, isWideNow, () => true)
  // stale guard: a slow page-1 reload must not land on top of a newer query
  const gen = useRef(0)

  const load = useCallback((search: string, before: string | null, replace: boolean) => {
    const g = gen.current
    setLoading(true)
    listBooks({ search, before, limit: PAGE })
      .then((page) => {
        if (gen.current !== g) return
        setBooks((cur) => {
          if (replace) return page.books
          const seen = new Set(cur.map((b) => b.id))
          return [...cur, ...page.books.filter((b) => !seen.has(b.id))]
        })
        setNextBefore(page.next_before)
        setLoaded(true)
      })
      .catch((e) => notify(errText(e)))
      .finally(() => {
        if (gen.current === g) setLoading(false)
      })
  }, [])

  const loadSide = useCallback(() => {
    currentlyReading()
      .then(setReading)
      .catch(() => setReading([]))
    listEreaders()
      .then((l) => setEreaders(activeEreaders(l)))
      .catch(() => setEreaders([]))
  }, [])
  useEffect(loadSide, [loadSide, dataVersion])

  // page 1 again when the query (debounced) or the store stamp moves
  const deb = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => {
    if (deb.current) clearTimeout(deb.current)
    deb.current = setTimeout(
      () => {
        gen.current++
        load(query, null, true)
      },
      query ? 250 : 0,
    )
    return () => {
      if (deb.current) clearTimeout(deb.current)
    }
  }, [query, dataVersion, load])

  // a finished upload run reloads page 1 (the stamp poll would too, later)
  const { runs } = useBookUploads()
  const firstRuns = useRef(runs)
  useEffect(() => {
    if (runs === firstRuns.current) return
    firstRuns.current = runs
    gen.current++
    load(query, null, true)
    loadSide()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runs])

  // the page-level drop guard: a file dropped anywhere on the Books page is
  // an upload, never the browser opening the file in place of the app.
  // The dropzone stops its own drops, so each drop is enqueued once.
  useEffect(() => {
    const hasFiles = (e: DragEvent) => !!e.dataTransfer && Array.from(e.dataTransfer.types).includes('Files')
    const over = (e: DragEvent) => {
      if (!hasFiles(e)) return
      e.preventDefault()
      if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy'
    }
    const drop = (e: DragEvent) => {
      if (!hasFiles(e)) return
      e.preventDefault()
      bookUploads.enqueue(e.dataTransfer?.files)
    }
    window.addEventListener('dragover', over)
    window.addEventListener('drop', drop)
    return () => {
      window.removeEventListener('dragover', over)
      window.removeEventListener('drop', drop)
    }
  }, [])

  // progress for the loaded ids, after every list change
  useEffect(() => {
    if (books.length === 0) {
      setProgress({})
      return
    }
    const g = gen.current
    fetchProgress(books.map((b) => b.id))
      .then((p) => {
        if (gen.current === g) setProgress(p)
      })
      .catch(() => {})
  }, [books])

  const patched = useCallback((next: Book) => {
    setBooks((cur) => cur.map((b) => (b.id === next.id ? next : b)))
    setReading((cur) => cur && cur.map((b) => (b.id === next.id ? { ...b, ...next } : b)))
  }, [])
  const deleted = useCallback((id: string) => {
    setBooks((cur) => cur.filter((b) => b.id !== id))
    setReading((cur) => cur && cur.filter((b) => b.id !== id))
    setSelected(null)
  }, [])

  // the server's order (newest first): the same order the pages arrive in,
  // so "Load more" appends below instead of reshuffling a partial page
  const matching = searchFilter(books, query)
  const shown = shelfFilter(matching, progress, shelf)

  // a reading card's book may not be on a loaded page: its progress rides on its row
  const progressFor = (id: string): BookProgress | null => {
    const p = progress[id]
    if (p) return p
    const r = reading?.find((b) => b.id === id)
    return r ? { percentage: r.percentage, device: r.device, updated_at: r.updated_at, status: 'reading' } : null
  }
  // on a wide window the details always show one book: the picked one, else the newest
  const pickedBook = selected ? (books.find((b) => b.id === selected) ?? reading?.find((b) => b.id === selected) ?? null) : null
  const detailBook = pickedBook ?? (wide ? (shown[0] ?? null) : null)

  const finished = useMemo(() => Object.values(progress).filter((p) => p?.status === 'finished').length, [progress])
  const lead = booksLead({ books: books.length, more: !!nextBefore, reading: reading?.length ?? 0, finished, ereaders: ereaders?.length ?? null })

  const detail = detailBook && (
    <BookDetail
      key={detailBook.id}
      book={detailBook}
      progress={progressFor(detailBook.id)}
      onPatched={patched}
      onDeleted={deleted}
      inSheet={!wide}
    />
  )

  return (
    <div className="bk-page">
      <div className="bk-main">
        <div className="bk-head">
          <div className="bk-head-text">
            <h1 className="bk-title">Books</h1>
            <div className="bk-lead">{loaded ? lead : ' '}</div>
          </div>
          <div className="bk-head-actions">
            <label htmlFor="bk-filter" className="sr-only">
              Filter by title, author or series
            </label>
            <input
              id="bk-filter"
              className="bk-filter"
              placeholder="Title, author or series"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Escape' && query) {
                  e.stopPropagation()
                  setQuery('')
                }
              }}
              spellCheck={false}
            />
            {showColourPass(books) && (
              <Button size="sm" onClick={() => setReviewing(true)}>
                Find colour covers
              </Button>
            )}
            <Button variant="primary" size="sm" icon="upload" onClick={() => document.getElementById('book-upload-input')?.click()}>
              Add EPUBs
            </Button>
          </div>
        </div>

        {!query && <FromTaisce dataVersion={dataVersion} />}

        {reading && reading.length > 0 && !query && (
          <section aria-labelledby="bk-h-reading" className="bk-section">
            <h2 id="bk-h-reading" className="bk-h2">
              Currently reading
            </h2>
            <div className="bk-reading">
              {reading.map((b) => {
                const pct = pctOf({ percentage: b.percentage, status: 'reading' })
                return (
                  <button
                    key={b.id}
                    type="button"
                    className="lp-card bk-rcard"
                    onClick={() => navigate({ kind: 'read', source: 'book', id: b.id })}
                    aria-label={`Read ${b.title}, ${pct}% through`}
                  >
                    <Cover book={b} size="card" />
                    <span className="bk-rcard-body">
                      <span className="bk-rcard-title">{b.title}</span>
                      {b.author && <span className="muted bk-rcard-author">{b.author}</span>}
                      <span className="bk-rcard-foot">
                        <span className="bk-rcard-line">
                          <b>{pct}%</b>
                          <span className="muted">
                            {b.device} · {relTime(b.updated_at)}
                          </span>
                        </span>
                        <Bar pct={pct} label={`${b.title}: ${pct}% read`} />
                      </span>
                    </span>
                  </button>
                )
              })}
            </div>
          </section>
        )}

        <BookUpload />

        <section aria-labelledby="bk-h-all" className="bk-section bk-library">
          <div className="bk-libhead">
            <h2 id="bk-h-all" className="bk-h2">
              Library
            </h2>
            <div className="bk-seg" role="group" aria-label="Show">
              {(['all', 'unread', 'finished'] as const).map((s) => (
                <button key={s} type="button" className={shelf === s ? 'on' : ''} aria-pressed={shelf === s} onClick={() => setShelf(s)}>
                  {s === 'all' ? 'All' : s === 'unread' ? 'Unread' : 'Finished'}
                </button>
              ))}
            </div>
            <span className="muted bk-order">Newest first</span>
          </div>

          {!loaded && loading && (
            <div className="bk-grid" aria-hidden>
              {Array.from({ length: 6 }, (_, i) => (
                <div key={i} className="bk-tile">
                  <span className="bk-cover bk-cover-grid lp-skeleton" />
                </div>
              ))}
            </div>
          )}

          {shown.length > 0 && (
            <div className="bk-grid">
              {shown.map((b) => {
                const p = progress[b.id] ?? null
                const badge = coverBadge(b, p)
                const on = detailBook?.id === b.id
                return (
                  <div key={b.id} className={`bk-tile${on ? ' on' : ''}`}>
                    {/* the cover opens the book; the words, its details */}
                    <Link to={{ kind: 'read', source: 'book', id: b.id }} className="bk-tile-cover" aria-label={`Read ${b.title}`} title={`Read ${b.title}`}>
                      <Cover book={b} size="grid" badge={badge} />
                      {p?.status === 'reading' && <Bar pct={pctOf(p)} label={`${pctOf(p)}% read`} />}
                    </Link>
                    <button
                      type="button"
                      className="bk-tile-info"
                      aria-pressed={on}
                      onClick={() => setSelected(b.id)}
                      title={b.author ? `${b.title} · ${b.author}` : b.title}
                    >
                      <span className="bk-tile-title">{b.title}</span>
                      {b.author && <span className="bk-tile-author">{b.author}</span>}
                    </button>
                  </div>
                )
              })}
            </div>
          )}

          {loaded && !loading && shown.length === 0 && (
            <EmptyState icon="books" title={query ? `Nothing matches “${query}”` : books.length === 0 ? 'No books yet' : shelf === 'finished' ? 'Nothing finished yet' : 'Nothing unread'}>
              {query
                ? 'Try part of a title, an author or a series.'
                : books.length === 0
                  ? 'Drop EPUBs anywhere on this page, or choose files above. They reach your e-readers at their next sync.'
                  : nextBefore
                    ? 'Among the books loaded so far. Load more to look further.'
                    : null}
            </EmptyState>
          )}

          {nextBefore && (
            <div className="bk-more">
              <Button size="sm" disabled={loading} onClick={() => load(query, nextBefore, false)}>
                {loading ? 'Loading…' : 'Load more'}
              </Button>
            </div>
          )}
        </section>
      </div>

      <aside className="bk-aside" aria-label="Book details and e-readers">
        {wide && (detail ?? (loaded && books.length === 0 ? null : <Card className="bk-detail bk-detail-empty">Pick a book to see its details.</Card>))}
        <EreadersCard list={ereaders} />
      </aside>

      {reviewing && (
        <CoverReviewSheet
          onClose={() => {
            setReviewing(false)
            // the pass judged covers shelved before detection: page 1 again
            gen.current++
            load(query, null, true)
          }}
          onChanged={patched}
        />
      )}

      {!wide && pickedBook && (
        <Sheet title="Book details" onClose={() => setSelected(null)}>
          {detail}
        </Sheet>
      )}
    </div>
  )
}

/** Under a sent book's title: "Work · 12 chapters · 1.2 MB". */
export function sentBookLine(b: Pick<SentBook, 'workspace' | 'chapters' | 'size'>): string {
  return [b.workspace ?? 'Unsorted', chaptersLabel(b.chapters), formatSize(b.size)].join(' · ')
}

/** The "From Taisce" shelf: GET /api/ereader/sent; hidden while it loads,
 * when it is empty, and on a daemon without the route. */
function FromTaisce({ dataVersion }: { dataVersion: number }) {
  const [list, setList] = useState<SentBook[] | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const load = useCallback(() => {
    listSentBooks()
      .then(setList)
      .catch(() => setList(null))
  }, [])
  useEffect(load, [load, dataVersion])
  const stop = async (b: SentBook) => {
    setBusy(b.id)
    try {
      await setSending(b.id, false)
      notify(`stopped sending “${b.title}”`, 'ok')
    } catch (e) {
      notify(errText(e))
    }
    setBusy(null)
    load()
  }
  const download = (b: SentBook) =>
    downloadEpub(b.id, b.title)
      .then((name) => notify(`downloaded ${name}`, 'ok'))
      .catch((e) => notify(errText(e)))
  return <SentShelf list={list} busy={busy} onDownload={download} onStop={stop} />
}

export function SentShelf({
  list,
  busy = null,
  onDownload,
  onStop,
}: {
  list: SentBook[] | null
  /** the book whose Stop sending is in flight */
  busy?: string | null
  onDownload: (b: SentBook) => void
  onStop: (b: SentBook) => void
}) {
  if (!list || list.length === 0) return null
  return (
    <section aria-labelledby="bk-h-sent" className="bk-section">
      <h2 id="bk-h-sent" className="bk-h2">
        From Taisce
      </h2>
      <div className="bk-reading">
        {list.map((b) => {
          const p = b.progress
          const pct = pctOf(p)
          return (
            <Card key={b.id} className="bk-rcard bk-sent">
              <span className="bk-rcard-body">
                <Link to={{ kind: 'doc', id: b.id }} className="bk-rcard-title bk-sent-title">
                  {b.title}
                </Link>
                <span className="muted bk-rcard-author">{sentBookLine(b)}</span>
                <span className="bk-rcard-foot">
                  {p && p.status !== 'unread' && (
                    <>
                      <span className="bk-rcard-line">
                        <b>{pct}%</b>
                        <span className="muted">{progressWhere(p)}</span>
                      </span>
                      <Bar pct={pct} label={`${b.title}: ${pct}% read`} />
                    </>
                  )}
                  <span className="bk-sent-actions">
                    <Link to={{ kind: 'read', source: 'doc', id: b.id }} className={buttonClass('primary', 'sm')}>
                      <Icon name="books" size={15} />
                      Read
                    </Link>
                    <Button size="sm" icon="download" onClick={() => onDownload(b)}>
                      Download
                    </Button>
                    <Button size="sm" variant="ghost" disabled={busy === b.id} onClick={() => onStop(b)}>
                      {busy === b.id ? 'Stopping…' : 'Stop sending'}
                    </Button>
                  </span>
                </span>
              </span>
            </Card>
          )
        })}
      </div>
    </section>
  )
}

function Bar({ pct, label }: { pct: number; label?: string }) {
  return (
    <span className="bk-bar" role="progressbar" aria-label={label ?? 'Reading progress'} aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
      <i style={{ width: `${pct}%` }} />
    </span>
  )
}

/** A cover: the book's own image, else (or when it fails to load) the
 * typographic one. `size` scales the type: grid, the reading card, the details. */
export function Cover({ book, size, badge }: { book: Pick<Book, 'id' | 'title' | 'author' | 'has_cover' | 'cover_version'>; size: 'grid' | 'card' | 'detail'; badge?: string | null }) {
  const [broken, setBroken] = useState(false)
  const art = coverArt(book.id)
  const img = book.has_cover && !broken
  const style = { '--cv-bg': art.bg, '--cv-fg': art.fg } as CSSProperties
  return (
    <span className={`bk-cover bk-cover-${size}${img ? ' bk-cover-img' : ''}${badge ? ' bk-cover-badged' : ''}`} style={style} aria-hidden>
      {img ? (
        <img loading="lazy" src={coverUrl(book.id, book.cover_version)} alt="" onError={() => setBroken(true)} />
      ) : (
        <>
          <span className="bk-cv-m" style={art.motif} />
          <span className="bk-cv-t">{book.title}</span>
          <span className="bk-cv-a">{book.author ?? ''}</span>
        </>
      )}
      {badge && <span className="bk-badge">{badge}</span>}
    </span>
  )
}

/** The edits a form asks for, as a PATCH: only the fields that changed.
 * Throws on a series number that is not a number. */
export function bookPatch(book: Book, form: { title: string; author: string; series: string; number: string }): BookPatch {
  const p: BookPatch = {}
  const title = form.title.trim()
  if (title && title !== book.title) p.title = title
  const author = form.author.trim()
  if (author !== (book.author ?? '')) p.author = author
  const series = form.series.trim()
  if (series !== (book.series ?? '')) p.series = series || null
  const idx = parseSeriesIndex(form.number)
  if (idx !== book.series_index) p.series_index = idx
  return p
}

function BookDetail({
  book,
  progress,
  onPatched,
  onDeleted,
  inSheet,
}: {
  book: Book
  progress: BookProgress | null
  onPatched: (b: Book) => void
  onDeleted: (id: string) => void
  inSheet: boolean
}) {
  const initial = useMemo(
    () => ({ title: book.title, author: book.author ?? '', series: book.series ?? '', number: book.series_index === null ? '' : String(book.series_index) }),
    [book],
  )
  const [form, setForm] = useState(initial)
  const [busy, setBusy] = useState(false)
  useEffect(() => setForm(initial), [initial])
  const dirty = form.title !== initial.title || form.author !== initial.author || form.series !== initial.series || form.number !== initial.number
  const set = (k: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm((f) => ({ ...f, [k]: e.target.value }))

  const save = async () => {
    if (busy || !dirty) return
    let patch: BookPatch
    try {
      // a non-number is refused here; an out-of-range one by the server
      patch = bookPatch(book, form)
    } catch (e) {
      notify(errText(e))
      return
    }
    if (Object.keys(patch).length === 0) {
      setForm(initial)
      return
    }
    setBusy(true)
    try {
      onPatched(await updateBook(book.id, patch))
      notify('saved', 'ok')
    } catch (e) {
      notify(errText(e))
    }
    setBusy(false)
  }

  const doDelete = async () => {
    try {
      await deleteBook(book.id)
      notify(`deleted “${book.title}”`, 'ok')
      onDeleted(book.id)
    } catch (e) {
      notify(errText(e))
    }
  }

  const pct = pctOf(progress)
  const series = book.series ? `${book.series}${book.series_index !== null ? ` · #${book.series_index}` : ''}` : 'Not in a series'
  const fid = `bk-f-${book.id}`
  const body = (
    <>
      <div className="bk-d-top">
        <Cover book={book} size="detail" />
        <div className="bk-d-meta">
          <span className="bk-d-title">{book.title}</span>
          {book.author && <span className="bk-d-author">{book.author}</span>}
          <span className="muted bk-d-series">{series}</span>
          <span className="bk-d-progress">
            {progress && progress.status !== 'unread' ? (
              <>
                <b>{pct}%</b> · {progressWhere(progress)}
              </>
            ) : (
              'Not started'
            )}
          </span>
          <Bar pct={pct} />
          <Link to={{ kind: 'read', source: 'book', id: book.id }} className={buttonClass('primary', 'sm', 'bk-d-read')}>
            <Icon name="books" size={15} />
            {progress && progress.status === 'reading' ? 'Continue reading' : 'Read'}
          </Link>
        </div>
      </div>
      <form
        className="bk-d-form"
        onSubmit={(e) => {
          e.preventDefault()
          save()
        }}
      >
        <div className="bk-fld bk-fld-wide">
          <label htmlFor={`${fid}-t`}>Title</label>
          <input id={`${fid}-t`} value={form.title} onChange={set('title')} />
        </div>
        <div className="bk-fld bk-fld-wide">
          <label htmlFor={`${fid}-a`}>Author</label>
          <input id={`${fid}-a`} value={form.author} onChange={set('author')} />
        </div>
        <div className="bk-fld">
          <label htmlFor={`${fid}-s`}>Series</label>
          <input id={`${fid}-s`} value={form.series} onChange={set('series')} placeholder="None" />
        </div>
        <div className="bk-fld">
          <label htmlFor={`${fid}-n`}>Number</label>
          <input id={`${fid}-n`} value={form.number} onChange={set('number')} placeholder="—" inputMode="decimal" />
        </div>
        {/* Enter in a field saves */}
        <button type="submit" hidden tabIndex={-1} />
      </form>
      <div className="muted mono bk-d-file">
        {book.original_filename} · {formatSize(book.size)} · added {relTime(book.added_at)}
      </div>
      <p className="muted bk-d-note">Changing the title or author renames the file on your e-readers at their next download.</p>
      <ColourCoverPanel book={book} onPatched={onPatched} />
      <div className="bk-d-actions">
        <Button variant="primary" size="sm" disabled={!dirty || busy} onClick={save}>
          {busy ? 'Saving…' : 'Save changes'}
        </Button>
        {dirty && (
          <Button variant="ghost" size="sm" onClick={() => setForm(initial)}>
            Discard
          </Button>
        )}
        <span className="bk-d-del">
          <ConfirmButton confirmLabel="Delete for good" onConfirm={doDelete}>
            Delete…
          </ConfirmButton>
        </span>
      </div>
    </>
  )
  return inSheet ? (
    <div className="bk-detail bk-detail-sheet">{body}</div>
  ) : (
    <Card as="section" className="bk-detail" aria-label="Book details">
      {body}
    </Card>
  )
}

function EreadersCard({ list }: { list: Ereader[] | null }) {
  const now = Date.now()
  return (
    <Card as="section" className="bk-dev" aria-labelledby="bk-h-dev">
      <div className="bk-dev-head">
        <h2 id="bk-h-dev">E-readers</h2>
        <Link to={{ kind: 'settings', section: 'ereaders' }} className="bk-dev-add">
          Add an e-reader
        </Link>
      </div>
      {list === null && <Skeleton height={40} style={{ margin: '8px 0 12px' }} />}
      {list !== null && list.length === 0 && <div className="muted bk-dev-none">None yet. Add one in Settings and your books reach it over a catalog feed.</div>}
      {list?.map((d) => {
        const recent = !!d.last_used_at && now - Date.parse(d.last_used_at) < 86_400_000
        return (
          <div key={d.id} className="bk-dev-row">
            <DeviceIcon />
            <span className="bk-dev-text">
              <b>{d.label}</b>
              <span className="muted">
                {d.last_used_at ? `synced ${relTime(d.last_used_at)}` : 'never synced'}
                {d.stream_downloads ? ' · streams downloads' : ''}
              </span>
            </span>
            <span className={`bk-dev-dot${recent ? ' on' : ''}`} title={recent ? 'Synced in the last day' : 'Idle'} aria-label={recent ? 'Synced in the last day' : 'Idle'} role="img" />
          </div>
        )
      })}
    </Card>
  )
}
