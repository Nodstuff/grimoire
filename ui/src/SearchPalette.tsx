// ⌘⇧F Search: full-text block hits (each with its doc title, the heading when
// the hit is one, and a one-line snippet), then a trailing "Ask the library"
// row. A leading `?` is ask-only (⌘/ opens it that way). ⌘Enter asks from
// any row. No docs-by-title (⌘O), no actions (⌘K). Hits settle on one 120 ms
// tick; the previous hits hold their place, dimmed, until the next land, and
// the selected row keeps its identity (omni.ts stableSelection).

import { useEffect, useMemo, useRef, useState } from 'react'
import PaletteShell, { PaletteFooter, PaletteInput, moveSelection } from './PaletteShell'
import { Icon } from './lapis/icons'
import { navigate } from './router'
import { api, type SearchHit } from './types'
import type { OpenDoc } from './App'
import { buildSearchRows, parseSearch, stableSelection, type SearchRow } from './omni'

/** Search starts at this many characters. */
const MIN_CHARS = 2

export default function SearchPalette({
  initial = '',
  onOpenDoc,
  onClose,
  inline = false,
  onQuery,
}: {
  /** prefilled query: `?` from ⌘/, or the text ⌘O handed over */
  initial?: string
  onOpenDoc: OpenDoc
  onClose: () => void
  /** render in the page (the /search route) instead of as a palette */
  inline?: boolean
  /** the query as typed (the /search route keeps ?q= in step) */
  onQuery?: (q: string) => void
}) {
  const [raw, setRawState] = useState(initial)
  const setRaw = (v: string) => {
    setRawState(v)
    onQuery?.(v)
  }
  const [hits, setHits] = useState<SearchHit[]>([])
  const [settled, setSettled] = useState(() => parseSearch(''))
  const [pending, setPending] = useState(false)
  const [sel, setSel] = useState(0)
  const asking = false
  const inputRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const el = inputRef.current
    if (!el) return
    el.focus()
    el.setSelectionRange(el.value.length, el.value.length)
  }, [])

  useEffect(() => {
    const p = parseSearch(raw)
    if (p.ask || p.q.length < MIN_CHARS) {
      setSettled(p)
      setHits([])
      setPending(false)
      return
    }
    setPending(true)
    let stale = false
    const t = setTimeout(() => {
      setSettled(p)
      api<SearchHit[]>(`/api/search?q=${encodeURIComponent(p.q)}`)
        .then((hs) => {
          if (stale) return
          setHits(Array.isArray(hs) ? hs : [])
          setPending(false)
        })
        .catch(() => {
          if (stale) return
          setHits([])
          setPending(false)
        })
    }, 120)
    return () => {
      stale = true
      clearTimeout(t)
    }
  }, [raw])

  const { ask: askOnly, q } = settled
  const rows: SearchRow[] = useMemo(
    () => buildSearchRows({ ask: askOnly, q: q.length < MIN_CHARS && !askOnly ? '' : q, hits }),
    [askOnly, q, hits],
  )
  const prevRows = useRef<SearchRow[]>([])
  const prevQuery = useRef(`${askOnly}:${q}`)
  useEffect(() => {
    const key = `${askOnly}:${q}`
    if (key !== prevQuery.current) {
      prevQuery.current = key
      setSel(0)
    } else {
      setSel((s) => stableSelection(prevRows.current, s, rows))
    }
    prevRows.current = rows
  }, [rows, askOnly, q])
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>(`[data-idx="${sel}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [sel])

  const live = parseSearch(raw)
  const contentPending = pending || (!live.ask && live.q.length >= MIN_CHARS && live.q !== q)

  // asking lands on the Search & Ask page, which shows the answer with its
  // citations and receipts (canvas Search) and keeps it under Answers
  const ask = (question: string) => {
    const qq = question.trim()
    if (!qq) return
    onClose()
    navigate({ kind: 'search', q: qq }, { state: { ask: true } })
  }

  const run = (r: SearchRow | undefined) => {
    if (!r) return
    if (r.group === 'content') onOpenDoc(r.hit.block.doc_id, { blockId: r.hit.block.id })
    else ask(r.query || q)
  }

  const contentRows = rows.filter((r) => r.group === 'content').length
  const emptyText = askOnly
    ? null
    : live.q.length < MIN_CHARS
      ? 'type to search inside every doc — typos fine · start with ? to ask'
      : !contentPending && contentRows === 0
        ? 'no blocks match'
        : null

  return (
    <Frame inline={inline} onClose={onClose} locked={asking}>
      <PaletteInput
        icon={live.ask ? 'ask' : 'search'}
        ref={inputRef}
        placeholder={live.ask ? 'Ask your notes a question… every claim cites its block' : 'Search inside docs…  (? to ask)'}
        aria-label="Search inside docs"
        value={raw}
        disabled={asking}
        onChange={(e) => setRaw(e.target.value)}
        onKeyDown={(e) => {
          if (moveSelection(e, rows.length, setSel)) return
          if (e.key === 'Enter') {
            e.preventDefault()
            // ⌘Enter asks from anywhere in the list
            if (e.metaKey || e.ctrlKey) ask(live.q)
            else run(rows[sel])
          }
        }}
      />
      <div className="palette-list omni-list" ref={listRef} role="listbox">
        {asking && <div className="palette-empty">Reading your notes and writing the answer… (up to a minute)</div>}
        {!asking && emptyText && <div className="palette-empty">{emptyText}</div>}
        {!asking && contentPending && contentRows === 0 && <div className="omni-pending" aria-hidden />}
        {!asking &&
          rows.map((r, i) => (
            <div key={r.key}>
              {r.group === 'ask' && contentRows > 0 && <div className="omni-group">Ask</div>}
              <div
                data-idx={i}
                role="option"
                aria-selected={i === sel}
                className={`palette-item omni-${r.group} ${i === sel ? 'sel' : ''} ${r.group === 'content' && contentPending ? 'pending' : ''}`}
                onMouseMove={() => sel !== i && setSel(i)}
                onClick={() => run(r)}
              >
                {r.group === 'content' ? (
                  <div className="hit-body">
                    <span className="hit-doc">{r.crumb}</span>
                    <span className="hit-text">{r.snippet}</span>
                  </div>
                ) : (
                  <>
                    <span className="omni-ico agent">
                      <Icon name="ask" size={15} />
                    </span>
                    <span className="hit-body">
                      <span>
                        Ask the library: <em>“{r.query || '…'}”</em>
                      </span>
                    </span>
                    <span className="hint">an answer doc with citations</span>
                  </>
                )}
              </div>
            </div>
          ))}
      </div>
      <PaletteFooter
        hints={[
          ['↵', 'open'],
          ['⌘↵', 'ask'],
          ['?', 'ask only'],
          ['⌘O', 'open by title'],
          ['esc', 'close'],
        ]}
      />
    </Frame>
  )
}

/** A palette, or (inline) the same body as a card in the page. */
function Frame({ inline, onClose, locked, children }: { inline: boolean; onClose: () => void; locked: boolean; children: React.ReactNode }) {
  if (!inline)
    return (
      <PaletteShell onClose={onClose} locked={locked}>
        {children}
      </PaletteShell>
    )
  return <div className="palette lp-palette-inline">{children}</div>
}
