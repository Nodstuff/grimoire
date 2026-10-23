// The EPUB library (view kind 'books'): an upload dropzone up top, a search
// box, then a cover grid with reading progress — "load more" pages through
// next_before. A click opens the book's detail card: inline-editable
// title/author/series metadata (PATCH on save), the file facts, progress,
// and an armed delete. Covers come from /api/books/{id}/cover.

import { useCallback, useEffect, useRef, useState } from 'react'
import { errText, notify } from './Notice'
import { relTime } from './time'
import BookUpload from './BookUpload'
import {
  Book,
  BookPatch,
  BookProgress,
  coverUrl,
  deleteBook,
  fetchProgress,
  formatSize,
  initialsOf,
  listBooks,
  progressLabel,
  searchFilter,
  sortByTitle,
  updateBook,
} from './books'

const PAGE = 60

export default function Books({ dataVersion }: { dataVersion: number }) {
  const [books, setBooks] = useState<Book[]>([])
  const [nextBefore, setNextBefore] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [loading, setLoading] = useState(false)
  const [loaded, setLoaded] = useState(false)
  const [selected, setSelected] = useState<string | null>(null)
  const [progress, setProgress] = useState<Record<string, BookProgress | null>>({})
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
  }, [])
  const deleted = useCallback((id: string) => {
    setBooks((cur) => cur.filter((b) => b.id !== id))
    setSelected(null)
  }, [])

  const shown = sortByTitle(searchFilter(books, query))
  const selectedBook = selected ? (books.find((b) => b.id === selected) ?? null) : null

  return (
    <div className="queue">
      <h1 className="queue-title">books</h1>

      <BookUpload onUploaded={() => load(query, null, true)} />

      <input
        className="tree-filter"
        placeholder="filter by title, author, series…"
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

      {selectedBook && (
        <BookDetail
          key={selectedBook.id}
          book={selectedBook}
          progress={progress[selectedBook.id] ?? null}
          onPatched={patched}
          onDeleted={deleted}
          onClose={() => setSelected(null)}
        />
      )}

      {shown.length > 0 && (
        <div className="book-grid">
          {shown.map((b) => (
            <Cover key={b.id} book={b} progress={progress[b.id] ?? null} onOpen={() => setSelected(b.id)} />
          ))}
        </div>
      )}

      {loaded && !loading && shown.length === 0 && (
        <div className="palette-empty">
          {query ? `nothing matches “${query}”` : 'no books yet — drop EPUBs on the area above to start the library'}
        </div>
      )}

      {nextBefore && (
        <button className="chip book-more" disabled={loading} onClick={() => load(query, nextBefore, false)}>
          {loading ? 'loading…' : 'load more'}
        </button>
      )}
      {!nextBefore && loading && <div className="palette-empty">loading…</div>}
    </div>
  )
}

function Cover({
  book,
  progress,
  onOpen,
}: {
  book: Book
  progress: BookProgress | null
  onOpen: () => void
}) {
  const p = progress && progress.status !== 'unread' ? progress : null
  return (
    <button className="book" onClick={onOpen} title={book.author ? `${book.title} — ${book.author}` : book.title}>
      <span className="book-cover">
        {book.has_cover ? (
          <img loading="lazy" src={coverUrl(book.id)} alt="" />
        ) : (
          <span className="book-fallback">
            <span className="book-fallback-title">{book.title}</span>
            <span className="book-fallback-initials">{initialsOf(book.author ?? book.title)}</span>
          </span>
        )}
        {p && <span className={`badge book-badge ${p.status}`}>{progressLabel(p)}</span>}
      </span>
      <span className="book-title">{book.title}</span>
      {p && (
        <span className="book-progress">
          <span style={{ width: `${p.status === 'finished' ? 100 : Math.max(0, Math.min(100, p.percentage))}%` }} />
        </span>
      )}
    </button>
  )
}

/** One metadata row: click the value to edit it in place; Enter/blur saves,
 * Esc cancels (the DocTitle dance). */
function EditField({
  label,
  value,
  placeholder = '—',
  onSave,
}: {
  label: string
  value: string
  placeholder?: string
  onSave: (v: string) => Promise<void>
}) {
  const [editing, setEditing] = useState(false)
  const [v, setV] = useState(value)
  const saving = useRef(false)

  const save = async () => {
    if (saving.current) return
    saving.current = true
    setEditing(false)
    const next = v.trim()
    if (next === value) {
      setV(value)
      saving.current = false
      return
    }
    try {
      await onSave(next)
    } catch (e) {
      notify(errText(e))
      setV(value)
    } finally {
      saving.current = false
    }
  }

  if (!editing)
    return (
      <div className="profile-kv">
        <span className="meta">{label}</span>
        <span
          className="book-edit-val"
          title="click to edit"
          onClick={() => {
            setV(value)
            setEditing(true)
          }}
        >
          {value || <span className="meta dim">{placeholder}</span>}
        </span>
      </div>
    )
  return (
    <div className="profile-kv">
      <span className="meta">{label}</span>
      <input
        className="join-input book-edit"
        autoFocus
        value={v}
        onChange={(e) => setV(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') save()
          if (e.key === 'Escape') {
            setV(value)
            setEditing(false)
          }
        }}
        onBlur={save}
      />
    </div>
  )
}

function BookDetail({
  book,
  progress,
  onPatched,
  onDeleted,
  onClose,
}: {
  book: Book
  progress: BookProgress | null
  onPatched: (b: Book) => void
  onDeleted: (id: string) => void
  onClose: () => void
}) {
  const [armed, setArmed] = useState(false)
  const armTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(
    () => () => {
      if (armTimer.current) clearTimeout(armTimer.current)
    },
    [],
  )

  const patch = async (p: BookPatch) => {
    onPatched(await updateBook(book.id, p))
  }

  // arm-then-confirm (window.confirm is a no-op in the Tauri webview)
  const doDelete = async () => {
    if (!armed) {
      setArmed(true)
      if (armTimer.current) clearTimeout(armTimer.current)
      armTimer.current = setTimeout(() => setArmed(false), 2500)
      return
    }
    setArmed(false)
    try {
      await deleteBook(book.id)
      notify(`deleted “${book.title}”`, 'ok')
      onDeleted(book.id)
    } catch (e) {
      notify(errText(e))
    }
  }

  const seriesIndex = book.series_index === null ? '' : String(book.series_index)
  const saveSeriesIndex = async (v: string) => {
    if (v === '') return patch({ series_index: null })
    const n = Number(v)
    if (!Number.isFinite(n)) throw new Error('series # must be a number')
    return patch({ series_index: n })
  }

  return (
    <div className="card book-detail">
      <div className="card-head">
        <span className="card-doc">{book.title}</span>
        <span className="card-meta">added {relTime(book.added_at)}</span>
      </div>
      <div className="book-detail-body">
        <span className="book-cover book-detail-cover">
          {book.has_cover ? (
            <img src={coverUrl(book.id)} alt="" />
          ) : (
            <span className="book-fallback">
              <span className="book-fallback-title">{book.title}</span>
              <span className="book-fallback-initials">{initialsOf(book.author ?? book.title)}</span>
            </span>
          )}
        </span>
        <div className="book-detail-fields">
          <EditField label="title" value={book.title} onSave={(v) => patch({ title: v })} />
          <EditField label="author" value={book.author ?? ''} onSave={(v) => patch({ author: v || null })} />
          <EditField label="series" value={book.series ?? ''} onSave={(v) => patch({ series: v || null })} />
          <EditField label="series #" value={seriesIndex} onSave={saveSeriesIndex} />
          <div className="profile-kv">
            <span className="meta">file</span>
            <span className="mono book-file">{book.original_filename}</span>
          </div>
          <div className="profile-kv">
            <span className="meta">size</span>
            <span className="mono">{formatSize(book.size)}</span>
          </div>
          {progress && progress.status !== 'unread' && (
            <div className="profile-kv">
              <span className="meta">progress</span>
              <span>
                {progressLabel(progress)}
                {progress.status === 'reading' ? ' read' : ''} · {progress.device} · {relTime(progress.updated_at)}
              </span>
            </div>
          )}
          <div className="actions">
            <button className={`decline ${armed ? 'armed' : ''}`} onClick={doDelete}>
              {armed ? 'sure? delete for good' : 'delete…'}
            </button>
            <button className="chip open-in-doc" onClick={onClose}>
              close
            </button>
          </div>
        </div>
      </div>
    </div>
  )
}
