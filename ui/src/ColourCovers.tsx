// Colour covers (covers.rs): a book exported from a Kindle often wears a
// black-and-white cover; Taisce can show a colour one from Open Library
// instead, stored beside the book (the EPUB is never touched; the e-ink
// readers keep reading the file as it is). Three places offer it:
//
// - ColourCoverPanel, in a book's details: "Find a colour cover…" lays up
//   to four candidates beside the current cover; "Use this cover" or "Keep
//   mine"; "Search with a different title…" (a title and an optional
//   author the server searches instead); "Revert to the book's own cover"
//   while a colour one shows.
// - UploadColourOffer, under an upload result whose cover is greyscale: the
//   top match and "Use colour cover".
// - CoverReviewSheet, the one-off library pass behind Books' "Find colour
//   covers": every greyscale book with its best match, Accept / Skip each,
//   "Accept all good matches". An accept never overwrites a cover the person
//   chose (the server refuses it, 409 cover_chosen).
//
// The server does every Open Library request (one or two at a time); this
// file asks it one book at a time too.

import { useCallback, useEffect, useRef, useState } from 'react'
import { errText, notify } from './Notice'
import { Button, Sheet, Spinner, TextField } from './lapis'
import {
  Book,
  CoverCandidate,
  CoverReviewRow,
  type CoverSearchTerms,
  chooseCover,
  coverReview,
  coverSearchTerms,
  coverUrl,
  findCoverCandidates,
  goodMatches,
  oneAtATime,
  scanCoverReview,
  wantsColourCover,
  withoutSeries,
} from './books'
import { ApiError } from './types'

/** Lookups from this page run one after another, whichever view asked. */
let lane: Promise<unknown> = Promise.resolve()
export function serially<T>(fn: () => Promise<T>): Promise<T> {
  const p = lane.then(fn, fn)
  lane = p.catch(() => {})
  return p
}

/** "Open Library · this edition" / "Open Library · title match", and size. */
export function candidateLabel(c: Pick<CoverCandidate, 'source' | 'width' | 'height'>): string {
  return `${c.source === 'isbn' ? 'This edition' : 'Title match'} · ${c.width}×${c.height}`
}

function Thumb({ src, label, on, onPick }: { src: string | null; label: string; on?: boolean; onPick?: () => void }) {
  const img = src ? <img src={src} alt="" loading="lazy" /> : <span className="bk-cc-none">No cover</span>
  if (!onPick) {
    return (
      <span className="bk-cc-thumb">
        <span className="bk-cc-img">{img}</span>
        <span className="bk-cc-label">{label}</span>
      </span>
    )
  }
  return (
    <button type="button" className={`bk-cc-thumb bk-cc-pick${on ? ' on' : ''}`} aria-pressed={!!on} onClick={onPick} title={label}>
      <span className="bk-cc-img">{img}</span>
      <span className="bk-cc-label">{label}</span>
    </button>
  )
}

/** "Search with a different title…": a title and an optional author. */
export function DifferentTitleForm({ book, busy, onSearch, onCancel }: { book: Book; busy: boolean; onSearch: (t: CoverSearchTerms) => void; onCancel: () => void }) {
  const [title, setTitle] = useState(() => withoutSeries(book.title))
  const [author, setAuthor] = useState(book.author ?? '')
  const terms = coverSearchTerms(title, author)
  return (
    <form
      className="bk-cc-alt"
      aria-label="Search with a different title"
      onSubmit={(e) => {
        e.preventDefault()
        if (terms && !busy) onSearch(terms)
      }}
    >
      <TextField label="Title" value={title} maxLength={200} autoFocus onChange={(e) => setTitle(e.target.value)} />
      <TextField label="Author (optional)" value={author} maxLength={200} onChange={(e) => setAuthor(e.target.value)} />
      <div className="bk-d-actions">
        <Button type="submit" variant="primary" size="sm" icon="search" disabled={!terms || busy}>
          Search
        </Button>
        <Button size="sm" variant="ghost" disabled={busy} onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </form>
  )
}

/** In a book's details: find, compare, choose, revert. */
export function ColourCoverPanel({ book, onPatched }: { book: Book; onPatched: (b: Book) => void }) {
  const [open, setOpen] = useState(false)
  const [alt, setAlt] = useState(false)
  const [searched, setSearched] = useState<string | null>(null)
  const [busy, setBusy] = useState<'find' | 'choose' | null>(null)
  const [list, setList] = useState<CoverCandidate[] | null>(null)
  const [unavailable, setUnavailable] = useState(false)
  const [picked, setPicked] = useState<string | null>(null)
  const alive = useRef(true)
  useEffect(
    () => () => {
      alive.current = false
    },
    [],
  )

  const find = async (refresh: boolean, terms?: CoverSearchTerms) => {
    setOpen(true)
    setBusy('find')
    try {
      const r = await serially(() => findCoverCandidates(book.id, refresh, terms))
      if (!alive.current) return
      setAlt(false)
      setSearched(terms ? terms.title : null)
      setList(r.candidates)
      setUnavailable(r.unavailable)
      setPicked(r.candidates[0]?.id ?? null)
    } catch (e) {
      if (alive.current) notify(errText(e))
    }
    if (alive.current) setBusy(null)
  }

  const choose = async (pick: { candidate: string } | 'embedded', done: string) => {
    setBusy('choose')
    try {
      onPatched(await chooseCover(book.id, pick))
      notify(done, 'ok')
      if (alive.current) {
        setOpen(false)
        setList(null)
      }
    } catch (e) {
      notify(errText(e))
    }
    if (alive.current) setBusy(null)
  }

  const current = book.has_cover ? coverUrl(book.id, book.cover_version) : null
  return (
    <div className="bk-cc" aria-label="Colour cover">
      {book.has_color_cover ? (
        <div className="bk-cc-row">
          <span className="muted bk-cc-note">Showing a colour cover from Open Library. The book itself is unchanged.</span>
          <Button size="sm" variant="ghost" disabled={busy !== null} onClick={() => choose('embedded', book.has_embedded_cover ? 'back to the book’s own cover' : 'colour cover removed')}>
            {book.has_embedded_cover ? 'Revert to the book’s own cover' : 'Remove the colour cover'}
          </Button>
        </div>
      ) : (
        !open && (
          <div className="bk-cc-row">
            {wantsColourCover(book) && <span className="muted bk-cc-note">This cover is black and white.</span>}
            <Button size="sm" icon="search" disabled={busy !== null} onClick={() => find(false)}>
              Find a colour cover…
            </Button>
          </div>
        )
      )}
      {open && (
        <div className="bk-cc-find">
          {busy === 'find' && (
            <div className="bk-cc-row muted">
              <Spinner size={14} /> Asking Open Library…
            </div>
          )}
          {busy !== 'find' && list && list.length === 0 && (
            <div className="bk-cc-row muted">
              {unavailable ? 'Open Library didn’t answer. Try again in a while.' : searched ? `Open Library has no colour cover for “${searched}”.` : 'Open Library has no colour cover for this book.'}
            </div>
          )}
          {busy !== 'find' && list && list.length > 0 && (
            <div className="bk-cc-strip" role="group" aria-label="Covers to choose from">
              <Thumb src={current} label="Current" />
              {list.map((c) => (
                <Thumb key={c.id} src={c.url} label={candidateLabel(c)} on={picked === c.id} onPick={() => setPicked(c.id)} />
              ))}
            </div>
          )}
          {alt && <DifferentTitleForm book={book} busy={busy !== null} onSearch={(t) => find(true, t)} onCancel={() => setAlt(false)} />}
          <div className="bk-d-actions">
            {list && list.length > 0 && (
              <Button variant="primary" size="sm" disabled={!picked || busy !== null} onClick={() => picked && choose({ candidate: picked }, 'colour cover set')}>
                {busy === 'choose' ? 'Saving…' : 'Use this cover'}
              </Button>
            )}
            <Button size="sm" disabled={busy !== null} onClick={() => choose('embedded', 'keeping the book’s own cover')}>
              Keep mine
            </Button>
            <Button size="sm" variant="ghost" disabled={busy !== null} onClick={() => find(true)}>
              Search again
            </Button>
            {!alt && (
              <Button size="sm" variant="ghost" disabled={busy !== null} onClick={() => setAlt(true)}>
                Search with a different title…
              </Button>
            )}
            <Button
              size="sm"
              variant="ghost"
              disabled={busy !== null}
              onClick={() => {
                setOpen(false)
                setAlt(false)
              }}
            >
              Close
            </Button>
          </div>
        </div>
      )}
    </div>
  )
}

/** Under an upload result whose cover came out greyscale: the best match,
 * found by the server, and one button. Nothing when there is no match. */
export function UploadColourOffer({ book }: { book: Book }) {
  const [best, setBest] = useState<CoverCandidate | null>(null)
  const [state, setState] = useState<'looking' | 'offer' | 'busy' | 'done' | 'gone'>('looking')
  useEffect(() => {
    let live = true
    serially(() => findCoverCandidates(book.id))
      .then((r) => {
        if (!live) return
        const c = r.candidates[0] ?? null
        setBest(c)
        setState(c ? 'offer' : 'gone')
      })
      .catch(() => live && setState('gone'))
    return () => {
      live = false
    }
  }, [book.id])
  if (state === 'gone' || state === 'looking') return null
  if (state === 'done') return <span className="bk-cc-offer muted">colour cover set</span>
  return (
    <span className="bk-cc-offer">
      {best && <img className="bk-cc-mini" src={best.url} alt="" />}
      <Button
        size="sm"
        variant="ok"
        disabled={state === 'busy'}
        onClick={async () => {
          if (!best) return
          setState('busy')
          try {
            await chooseCover(book.id, { candidate: best.id }, true)
            setState('done')
          } catch (e) {
            notify(errText(e))
            setState('offer')
          }
        }}
      >
        Use colour cover
      </Button>
    </span>
  )
}

type Settled = 'accepted' | 'skipped' | 'kept'

/** The library pass: look at every cover shelved before detection, then
 * find each greyscale book's best match (one book at a time) and let the
 * person take or leave each. */
export function CoverReviewSheet({ onClose, onChanged }: { onClose: () => void; onChanged: (b: Book) => void }) {
  const [rows, setRows] = useState<CoverReviewRow[] | null>(null)
  const [scanLeft, setScanLeft] = useState<number | null>(null)
  const [settled, setSettled] = useState<Record<string, Settled>>({})
  const [busy, setBusy] = useState<string | null>(null)
  const [finding, setFinding] = useState(false)
  const stopped = useRef(false)
  useEffect(
    () => () => {
      stopped.current = true
    },
    [],
  )

  const setRow = useCallback((id: string, patch: Partial<CoverReviewRow>) => {
    setRows((cur) => cur && cur.map((r) => (r.book.id === id ? { ...r, ...patch } : r)))
  }, [])

  useEffect(() => {
    ;(async () => {
      try {
        let r = await coverReview()
        // covers shelved before detection: a batch per request until none
        while (r.unscanned > 0 && !stopped.current) {
          setScanLeft(r.unscanned)
          const next = await scanCoverReview()
          if ((next.scanned ?? 0) === 0) break
          r = next
        }
        if (stopped.current) return
        setScanLeft(null)
        setRows(r.books)
        const todo = r.books.filter((x) => !x.looked_up)
        setFinding(todo.length > 0)
        await oneAtATime(
          todo,
          async (row) => {
            try {
              const found = await serially(() => findCoverCandidates(row.book.id))
              if (!stopped.current) setRow(row.book.id, { looked_up: true, best: found.candidates[0] ?? null })
            } catch {
              if (!stopped.current) setRow(row.book.id, { looked_up: true, best: null })
            }
          },
          () => stopped.current,
        )
        if (!stopped.current) setFinding(false)
      } catch (e) {
        if (!stopped.current) {
          notify(errText(e))
          setRows([])
        }
      }
    })()
  }, [setRow])

  const accept = async (row: CoverReviewRow): Promise<void> => {
    if (!row.best) return
    setBusy(row.book.id)
    try {
      const b = await chooseCover(row.book.id, { candidate: row.best.id }, true)
      onChanged(b)
      setSettled((s) => ({ ...s, [row.book.id]: 'accepted' }))
    } catch (e) {
      if (e instanceof ApiError && e.code === 'cover_chosen') setSettled((s) => ({ ...s, [row.book.id]: 'kept' }))
      else notify(errText(e))
    }
    setBusy(null)
  }

  const good = rows ? goodMatches(rows, settled) : []
  const acceptAll = async () => {
    setBusy('all')
    await oneAtATime(good, accept, () => stopped.current)
    setBusy(null)
  }

  const footer = rows && rows.length > 0 && (
    <>
      <span className="muted bk-cc-foot">{finding ? 'Still asking Open Library…' : `${good.length} match${good.length === 1 ? '' : 'es'} to review`}</span>
      <Button variant="primary" size="sm" disabled={good.length === 0 || busy !== null} onClick={acceptAll}>
        {busy === 'all' ? 'Accepting…' : `Accept all good matches${good.length ? ` (${good.length})` : ''}`}
      </Button>
    </>
  )

  return (
    <Sheet title="Colour covers" onClose={onClose} footer={footer || undefined}>
      <div className="bk-cc-review">
        <p className="muted bk-cc-intro">Black-and-white covers, each beside the best colour match from Open Library. Accepting shows the colour cover here and in the catalog feed; the book file is never changed, and a cover you chose yourself is never replaced.</p>
        {scanLeft !== null && (
          <div className="bk-cc-row muted">
            <Spinner size={14} /> Looking at covers… {scanLeft} to go
          </div>
        )}
        {rows === null && scanLeft === null && (
          <div className="bk-cc-row muted">
            <Spinner size={14} /> Loading…
          </div>
        )}
        {rows && rows.length === 0 && <div className="bk-cc-row muted">No black-and-white covers left. Every book shows a colour cover, or the cover you chose.</div>}
        {rows && rows.length > 0 && (
          <ul className="bk-cc-list">
            {rows.map((r) => (
              <ReviewRow key={r.book.id} row={r} state={settled[r.book.id]} busy={busy === r.book.id || busy === 'all'} onAccept={() => accept(r)} onSkip={() => setSettled((s) => ({ ...s, [r.book.id]: 'skipped' }))} />
            ))}
          </ul>
        )}
      </div>
    </Sheet>
  )
}

/** What a settled row says instead of its buttons. */
export function settledText(s: Settled): string {
  return s === 'accepted' ? 'Colour cover set' : s === 'kept' ? 'Kept: you chose this cover' : 'Skipped'
}

function ReviewRow({ row, state, busy, onAccept, onSkip }: { row: CoverReviewRow; state?: Settled; busy: boolean; onAccept: () => void; onSkip: () => void }) {
  const b = row.book
  return (
    <li className={`bk-cc-item${state ? ` is-${state}` : ''}`}>
      <span className="bk-cc-pair">
        <Thumb src={b.has_cover ? coverUrl(b.id, b.cover_version) : null} label="Now" />
        <span className="bk-cc-arrow" aria-hidden>
          →
        </span>
        {row.best ? (
          <Thumb src={row.best.url} label={candidateLabel(row.best)} />
        ) : (
          <span className="bk-cc-thumb">
            <span className="bk-cc-img">{row.looked_up ? <span className="bk-cc-none">No match</span> : <Spinner size={14} />}</span>
            <span className="bk-cc-label">{row.looked_up ? 'Open Library' : 'Looking…'}</span>
          </span>
        )}
      </span>
      <span className="bk-cc-meta">
        <b>{b.title}</b>
        {b.author && <span className="muted">{b.author}</span>}
        <span className="bk-cc-acts">
          {state ? (
            <span className="muted">{settledText(state)}</span>
          ) : (
            <>
              <Button size="sm" variant="ok" disabled={!row.best || busy} onClick={onAccept}>
                Accept
              </Button>
              <Button size="sm" variant="ghost" disabled={busy} onClick={onSkip}>
                Skip
              </Button>
            </>
          )}
        </span>
      </span>
    </li>
  )
}
