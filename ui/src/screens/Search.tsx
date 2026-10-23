// /search?q=…: Search & Ask (canvas Search). One big field; In docs / Titles
// / Ask as a segmented switch with counts; filter chips (workspace, tags,
// who wrote it); the hits in a card (path › doc › heading, then the block in
// serif with the query marked); beside them, "Ask the library": the answer
// written from your own blocks, every claim a numbered citation, the quoted
// receipts under it. ↵ opens the first hit, ⌘↵ asks. An answer is a doc under
// Answers (ask.rs); the page reads it back and shows it here, and finds the
// saved answer again when the same question is asked twice.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Button, Icon, Kbd, AgentGlyph, Skeleton, SkeletonLines, Spinner } from '../lapis'
import Link from '../Link'
import { errText, notify } from '../Notice'
import { navigate } from '../router'
import { docPath, scoreDoc } from '../omni'
import { api, type Doc, type SearchHit } from '../types'
import { workspaceColor, type Workspace, type WorkspaceSel } from '../workspaces'
import { livingLabel, type LivingStatus } from '../living'
import type { OpenDoc } from '../App'
import { headingOf, loadTree, useDocTags } from './docCache'
import { highlight, sameQuestion, numberCitations, parseAnswer, snippetAround, type AnswerRun, type ParsedAnswer, type Seg } from './searchText'
import './search.css'

const MIN_CHARS = 2
const FIRST_PAGE = 6
type Mode = 'docs' | 'titles' | 'ask'
type Author = 'anyone' | 'people' | 'agents'

export interface AskResult {
  doc_id: string | null
  title: string
  sources: number
  docs: number
}

/** POST /api/ask: the answer doc (or doc_id null when nothing matched). */
export function askLibrary(question: string): Promise<AskResult> {
  return api<AskResult>('/api/ask', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ question }),
  })
}

function Marked({ segs }: { segs: Seg[] }) {
  return (
    <>
      {segs.map((s, i) => (s.m ? <mark key={i}>{s.t}</mark> : <span key={i}>{s.t}</span>))}
    </>
  )
}

/** A filter chip that opens a short menu of choices (radio items). */
function ChipMenu<T extends string>({
  label,
  value,
  options,
  onChange,
}: {
  label: string
  value: T
  options: { id: T; label: string; dot?: string }[]
  onChange: (v: T) => void
}) {
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLDivElement>(null)
  const btn = useRef<HTMLButtonElement>(null)
  useEffect(() => {
    if (!open) return
    const off = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false)
    }
    window.addEventListener('mousedown', off)
    box.current?.querySelector<HTMLElement>('[aria-checked="true"]')?.focus()
    return () => window.removeEventListener('mousedown', off)
  }, [open])
  const cur = options.find((o) => o.id === value)
  return (
    <div className="lp-chipmenu" ref={box}>
      <button
        ref={btn}
        type="button"
        className={`lp-srch-chip${value !== options[0]?.id ? ' on' : ''}`}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        {label}: {cur?.label ?? value}
        <Icon name="chevronDown" size={12} />
      </button>
      {open && (
        <div
          className="lp-chipmenu-list"
          role="menu"
          aria-label={label}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.stopPropagation()
              setOpen(false)
              btn.current?.focus()
              return
            }
            if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
            e.preventDefault()
            const items = Array.from(box.current?.querySelectorAll<HTMLElement>('[role="menuitemradio"]') ?? [])
            const i = items.indexOf(document.activeElement as HTMLElement)
            items[e.key === 'ArrowDown' ? Math.min(i + 1, items.length - 1) : Math.max(i - 1, 0)]?.focus()
          }}
        >
          {options.map((o) => (
            <button
              key={o.id}
              type="button"
              role="menuitemradio"
              aria-checked={o.id === value}
              onClick={() => {
                onChange(o.id)
                setOpen(false)
                btn.current?.focus()
              }}
            >
              {o.dot && <span className="lp-dot" style={{ background: o.dot }} aria-hidden />}
              <span>{o.label}</span>
              {o.id === value && <Icon name="check" size={14} className="lp-chipmenu-check" />}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

export default function Search({
  q: urlQ,
  onOpenDoc,
  docs,
  workspaces,
  dataVersion,
  askIntent = false,
  answerId = null,
}: {
  q: string
  onOpenDoc: OpenDoc
  docs: Doc[]
  workspaces: Workspace[]
  dataVersion: number
  /** arrived to ask (the ⌘⇧F palette's ⌘↵, ⌘/): ask `q` on mount */
  askIntent?: boolean
  /** the answer doc this entry showed (history.state), restored on back */
  answerId?: string | null
}) {
  const startsAsk = urlQ.trimStart().startsWith('?')
  const [raw, setRawState] = useState(startsAsk ? urlQ.trimStart().slice(1).trimStart() : urlQ)
  const [mode, setMode] = useState<Mode>(startsAsk || askIntent ? 'ask' : 'docs')
  const setRaw = (v: string) => {
    setRawState(v)
    navigate({ kind: 'search', q: v }, { replace: true })
  }
  const inputRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const el = inputRef.current
    if (!el) return
    el.focus()
    el.setSelectionRange(el.value.length, el.value.length)
  }, [])

  /* ---- filters ---- */
  const [ws, setWs] = useState<WorkspaceSel>('all')
  const [tag, setTag] = useState<string | null>(null)
  const [author, setAuthor] = useState<Author>('anyone')
  const [tags, setTags] = useState<[string, number][]>([])
  useEffect(() => {
    api<[string, number][]>('/api/tags')
      .then((t) => setTags(Array.isArray(t) ? t : []))
      .catch(() => setTags([]))
  }, [dataVersion])
  const docTags = useDocTags(dataVersion)
  const [kinds, setKinds] = useState<Map<string, string>>(new Map())
  useEffect(() => {
    api<{ id: string; kind: string }[]>('/api/principals')
      .then((ps) => setKinds(new Map((Array.isArray(ps) ? ps : []).map((p) => [p.id, p.kind]))))
      .catch(() => {})
  }, [])

  /* ---- hits ---- */
  const q = raw.trim()
  const [hits, setHits] = useState<SearchHit[]>([])
  const [hitsFor, setHitsFor] = useState('')
  const [pending, setPending] = useState(false)
  const [shown, setShown] = useState(FIRST_PAGE)
  const searchKey = `${q}\u0000${ws}`
  useEffect(() => {
    if (q.length < MIN_CHARS) {
      setHits([])
      setHitsFor(searchKey)
      setPending(false)
      return
    }
    setPending(true)
    let stale = false
    const t = setTimeout(() => {
      const wsParam = ws === 'all' ? '' : `&workspace=${encodeURIComponent(ws)}`
      api<SearchHit[]>(`/api/search?q=${encodeURIComponent(q)}${wsParam}`)
        .then((hs) => {
          if (stale) return
          setHits(Array.isArray(hs) ? hs : [])
          setHitsFor(searchKey)
          setShown(FIRST_PAGE)
          setPending(false)
        })
        .catch(() => {
          if (stale) return
          setHits([])
          setHitsFor(searchKey)
          setPending(false)
        })
    }, 140)
    return () => {
      stale = true
      clearTimeout(t)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searchKey, dataVersion])

  const byId = useMemo(() => new Map(docs.map((d) => [d.id, d])), [docs])
  const wsIndex = useMemo(() => new Map(workspaces.map((w, i) => [w.id, workspaceColor(w, i)])), [workspaces])
  const dotOf = (d: Doc | undefined) => (d?.workspace_id ? (wsIndex.get(d.workspace_id) ?? 'var(--line2)') : 'var(--line2)')

  const filtered = useMemo(
    () =>
      hits.filter((h) => {
        if (tag && !(docTags.get(h.block.doc_id) ?? []).includes(tag)) return false
        if (author !== 'anyone') {
          const k = kinds.get(h.block.created_by)
          if (author === 'agents' ? k !== 'agent' : k === 'agent') return false
        }
        return true
      }),
    [hits, tag, docTags, author, kinds],
  )

  // the heading each visible hit sits under (one tree fetch per doc, cached)
  const [headings, setHeadings] = useState<Map<string, string | null>>(new Map())
  useEffect(() => {
    let live = true
    const need = filtered.slice(0, shown).filter((h) => !headings.has(h.block.id))
    if (!need.length) return
    Promise.all(
      need.map((h) =>
        loadTree(h.block.doc_id, byId.get(h.block.doc_id)?.current_epoch ?? 0).then(
          (t) => [h.block.id, t ? headingOf(t.roots, h.block.id) : null] as const,
        ),
      ),
    ).then((pairs) => {
      if (!live) return
      setHeadings((m) => {
        const n = new Map(m)
        for (const [k, v] of pairs) n.set(k, v)
        return n
      })
    })
    return () => {
      live = false
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filtered, shown])

  const titleRows = useMemo(() => {
    if (q.length < 1) return []
    return docs
      .filter((d) => ws === 'all' || (ws === 'unsorted' ? !d.workspace_id : d.workspace_id === ws))
      .filter((d) => !tag || (docTags.get(d.id) ?? []).includes(tag))
      .map((d) => ({ d, path: docPath(d, byId), s: scoreDoc(q, d.title, docPath(d, byId)) }))
      .filter((r) => r.s !== null)
      .sort((a, b) => (b.s ?? 0) - (a.s ?? 0))
      .slice(0, 40)
  }, [q, docs, byId, ws, tag, docTags])

  const settled = hitsFor === searchKey && !pending

  /* ---- ask ---- */
  const answersRoot = useMemo(() => docs.find((d) => d.parent_id === null && d.title === 'Answers')?.id ?? null, [docs])
  const savedAnswer = useMemo(() => {
    if (!answersRoot || q.length < MIN_CHARS) return null
    // the newest answer to this question (ids are UUIDv7: they sort by time)
    const hits = docs.filter((d) => d.parent_id === answersRoot && sameQuestion(d.title, q))
    return hits.sort((a, b) => (a.id < b.id ? 1 : -1))[0]?.id ?? null
  }, [docs, answersRoot, q])

  type AskState =
    | { status: 'idle' }
    | { status: 'asking'; question: string; t0: number }
    | { status: 'ready'; question: string; docId: string; t0: number | null; sources: number | null; docs: number | null }
    | { status: 'none'; question: string }
    | { status: 'error'; question: string; error: string }
  const [askState, setAskState] = useState<AskState>(() =>
    answerId ? { status: 'ready', question: '', docId: answerId, t0: null, sources: null, docs: null } : { status: 'idle' },
  )
  const ask = useCallback(
    async (question: string) => {
      const qq = question.trim()
      if (!qq || askState.status === 'asking') return
      const t0 = Date.now()
      setAskState({ status: 'asking', question: qq, t0 })
      try {
        const a = await askLibrary(qq)
        if (!a.doc_id) {
          setAskState({ status: 'none', question: qq })
          return
        }
        setAskState({ status: 'ready', question: qq, docId: a.doc_id, t0, sources: a.sources, docs: a.docs })
        // this history entry remembers its answer (back returns to it)
        navigate({ kind: 'search', q: qq }, { replace: true, state: { answer: a.doc_id } })
      } catch (e) {
        setAskState({ status: 'error', question: qq, error: errText(e) })
      }
    },
    [askState.status],
  )
  // arrived to ask: ask once
  const asked = useRef(false)
  // the palette asked something else while this page was open
  useEffect(() => {
    if (askIntent && urlQ !== raw) {
      setRawState(urlQ)
      setMode('ask')
      asked.current = false
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [askIntent, urlQ])
  useEffect(() => {
    if (askIntent && !asked.current && q && !answerId) {
      asked.current = true
      ask(q)
    }
  }, [askIntent, q, answerId, ask])
  // the answer card follows the question: a typed-over query drops a stale
  // answer unless it is the saved answer to the new text
  const shownAnswer: AskState =
    askState.status === 'idle' || (askState.status === 'ready' && askState.question && !sameQuestion(askState.question, q))
      ? savedAnswer
        ? { status: 'ready', question: q, docId: savedAnswer, t0: null, sources: null, docs: null }
        : { status: 'idle' }
      : askState

  const followUp = () => {
    setRaw('')
    setMode('ask')
    setAskState({ status: 'idle' })
    inputRef.current?.focus()
  }

  /* ---- keys ---- */
  const focusHit = (i: number) => {
    const els = Array.from(listRef.current?.querySelectorAll<HTMLElement>('.lp-hit') ?? [])
    els[Math.max(0, Math.min(i, els.length - 1))]?.focus()
  }
  const onInputKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      e.preventDefault()
      if (e.metaKey || e.ctrlKey || mode === 'ask') {
        if (mode !== 'ask') setMode('ask')
        ask(q)
        return
      }
      if (mode === 'docs' && settled && filtered[0]) onOpenDoc(filtered[0].block.doc_id, { blockId: filtered[0].block.id })
      else if (mode === 'titles' && titleRows[0]) onOpenDoc(titleRows[0].d.id)
    } else if (e.key === 'ArrowDown') {
      e.preventDefault()
      focusHit(0)
    }
  }
  const onListKey = (e: React.KeyboardEvent) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
    e.preventDefault()
    const els = Array.from(listRef.current?.querySelectorAll<HTMLElement>('.lp-hit') ?? [])
    const i = els.indexOf(document.activeElement as HTMLElement)
    if (e.key === 'ArrowUp' && i <= 0) inputRef.current?.focus()
    else focusHit(e.key === 'ArrowDown' ? i + 1 : i - 1)
  }

  const wsOptions = [
    { id: 'all', label: 'All' },
    { id: 'unsorted', label: 'Unsorted', dot: 'var(--line2)' },
    ...workspaces.map((w, i) => ({ id: w.id, label: w.display_name || w.name, dot: workspaceColor(w, i) })),
  ]
  const tagChips = useMemo(() => {
    const top = tags.slice(0, 4).map(([t]) => t)
    if (tag && !top.includes(tag)) top.unshift(tag)
    return top
  }, [tags, tag])

  const docsCount = q.length >= MIN_CHARS && settled ? filtered.length : null
  const hitsShown = filtered.slice(0, shown)

  /* ---- render ---- */
  const results =
    mode === 'ask' ? null : (
      <section className="lp-card lp-srch-results" aria-label="Results" aria-busy={!settled}>
        <div ref={listRef} onKeyDown={onListKey}>
          {mode === 'docs' ? (
            q.length < MIN_CHARS ? (
              <div className="lp-srch-empty">Type to search inside every doc. Typos are fine; whole phrases rank first.</div>
            ) : !settled && hits.length === 0 ? (
              <div className="lp-srch-loading">
                <SkeletonLines lines={4} />
              </div>
            ) : filtered.length === 0 ? (
              <div className="lp-srch-empty">
                No blocks match{tag ? ` with #${tag}` : ''}
                {author !== 'anyone' ? ` written by ${author}` : ''}. Try fewer words, or ask the library instead.
              </div>
            ) : (
              <div className={settled ? '' : 'lp-srch-stale'}>
                {hitsShown.map((h) => {
                  const d = byId.get(h.block.doc_id)
                  const parents = d ? docPath(d, byId).split(' › ').slice(0, -1) : []
                  const heading = headings.get(h.block.id)
                  const isHeading = /^#{1,6}\s/.test(h.block.content)
                  return (
                    <Link
                      key={h.block.id}
                      to={{ kind: 'doc', id: h.block.doc_id, block: h.block.id }}
                      className="lp-hit"
                      onClick={(e) => {
                        if (e.metaKey || e.ctrlKey || e.shiftKey) return
                        e.preventDefault()
                        onOpenDoc(h.block.doc_id, { blockId: h.block.id })
                      }}
                    >
                      <div className="lp-hit-c">
                        <span className="lp-dot" style={{ background: dotOf(d) }} aria-hidden />
                        <span className="lp-hit-path">
                          {parents.map((p, i) => (
                            <span key={i}>{p} › </span>
                          ))}
                          <b>{h.doc_title}</b>
                          {heading && !isHeading && <span> › {heading}</span>}
                        </span>
                      </div>
                      <div className="lp-hit-s">
                        <Marked segs={highlight(snippetAround(h.block.content, q), q)} />
                      </div>
                    </Link>
                  )
                })}
                {(filtered.length > shown || filtered.length > 0) && (
                  <div className="lp-hit-foot">
                    {filtered.length > shown && (
                      <Button size="sm" onClick={() => setShown((n) => n + 10)}>
                        Show {Math.min(10, filtered.length - shown)} more
                      </Button>
                    )}
                    <span className="muted">↵ opens the doc scrolled to the block</span>
                  </div>
                )}
              </div>
            )
          ) : q.length < 1 ? (
            <div className="lp-srch-empty">Type part of a title or a path, typos fine.</div>
          ) : titleRows.length === 0 ? (
            <div className="lp-srch-empty">No titles match.</div>
          ) : (
            titleRows.map(({ d, path }) => {
              const parents = path.split(' › ').slice(0, -1)
              return (
                <Link
                  key={d.id}
                  to={{ kind: 'doc', id: d.id }}
                  className="lp-hit"
                  onClick={(e) => {
                    if (e.metaKey || e.ctrlKey || e.shiftKey) return
                    e.preventDefault()
                    onOpenDoc(d.id)
                  }}
                >
                  <div className="lp-hit-c">
                    <span className="lp-dot" style={{ background: dotOf(d) }} aria-hidden />
                    <span className="lp-hit-path">{parents.length ? parents.join(' › ') : 'Top level'}</span>
                  </div>
                  <div className="lp-hit-s lp-hit-title">
                    <Marked segs={highlight(d.title, q)} />
                  </div>
                </Link>
              )
            })
          )}
        </div>
      </section>
    )

  return (
    <div className="lp-page lp-srch">
      <h1 className="lp-srch-sr">Search &amp; Ask</h1>
      <div className={`lp-srch-field${mode === 'ask' ? ' ask' : ''}`}>
        <Icon name={mode === 'ask' ? 'ask' : 'search'} size={22} />
        <label htmlFor="lp-sq" className="lp-srch-sr">
          Search inside docs or ask a question
        </label>
        <input
          id="lp-sq"
          ref={inputRef}
          value={raw}
          placeholder={mode === 'ask' ? 'Ask your notes a question…' : 'Search inside docs, or ask a question'}
          onChange={(e) => {
            const v = e.target.value
            // the palette's `?` prefix still switches to Ask
            if (v.startsWith('?')) {
              setMode('ask')
              setRaw(v.slice(1).trimStart())
            } else setRaw(v)
          }}
          onKeyDown={onInputKey}
          spellCheck={false}
          autoComplete="off"
        />
        {pending && <Spinner size={16} label="Searching…" />}
        <span className="lp-srch-keys muted">
          {mode === 'ask' ? (
            <>
              <Kbd>↵</Kbd> ask
            </>
          ) : (
            <>
              <Kbd>↵</Kbd> search · <Kbd>⌘↵</Kbd> ask
            </>
          )}
        </span>
      </div>

      <div className="lp-srch-bar">
        <div className="lp-srch-seg" role="tablist" aria-label="Mode">
          {(
            [
              ['docs', 'In docs', docsCount],
              ['titles', 'Titles', q.length >= 1 ? titleRows.length : null],
              ['ask', 'Ask', null],
            ] as const
          ).map(([id, label, n]) => (
            <button
              key={id}
              type="button"
              role="tab"
              aria-selected={mode === id}
              className={mode === id ? 'on' : ''}
              onClick={() => setMode(id)}
            >
              {label}
              {n !== null && <span className="lp-srch-count">{n >= 20 && id === 'docs' ? '20+' : n}</span>}
            </button>
          ))}
        </div>
        <span className="lp-srch-rule" aria-hidden />
        <ChipMenu label="Workspace" value={ws} options={wsOptions} onChange={setWs} />
        {tagChips.map((t) => (
          <button
            key={t}
            type="button"
            className={`lp-srch-chip${tag === t ? ' on' : ''}`}
            aria-pressed={tag === t}
            onClick={() => setTag(tag === t ? null : t)}
          >
            #{t}
            {tag === t && <Icon name="close" size={12} />}
          </button>
        ))}
        <ChipMenu<Author>
          label="Written by"
          value={author}
          options={[
            { id: 'anyone', label: 'anyone' },
            { id: 'people', label: 'people' },
            { id: 'agents', label: 'agents' },
          ]}
          onChange={setAuthor}
        />
      </div>

      <div className={`lp-srch-cols${mode === 'ask' ? ' ask' : ''}`}>
        {results}
        <AskCard
          state={shownAnswer}
          q={q}
          docs={docs}
          byId={byId}
          dataVersion={dataVersion}
          onAsk={() => ask(q)}
          onFollowUp={followUp}
          onOpenDoc={onOpenDoc}
          wide={mode === 'ask'}
        />
      </div>
    </div>
  )
}

/* ---------- the answer ---------- */

function AskCard({
  state,
  q,
  byId,
  dataVersion,
  onAsk,
  onFollowUp,
  onOpenDoc,
  wide,
}: {
  state:
    | { status: 'idle' }
    | { status: 'asking'; question: string; t0: number }
    | { status: 'ready'; question: string; docId: string; t0: number | null; sources: number | null; docs: number | null }
    | { status: 'none'; question: string }
    | { status: 'error'; question: string; error: string }
  q: string
  docs: Doc[]
  byId: Map<string, Doc>
  dataVersion: number
  onAsk: () => void
  onFollowUp: () => void
  onOpenDoc: OpenDoc
  wide: boolean
}) {
  const docId = state.status === 'ready' ? state.docId : null
  const [parsed, setParsed] = useState<ParsedAnswer | null>(null)
  const [living, setLiving] = useState<LivingStatus | null>(null)
  const [doneIn, setDoneIn] = useState<number | null>(null)
  const epoch = docId ? (byId.get(docId)?.current_epoch ?? 0) : 0
  useEffect(() => {
    setParsed(null)
    setDoneIn(null)
  }, [docId])
  useEffect(() => {
    if (!docId) return
    let live = true
    loadTree(docId, epoch * 1000 + dataVersion).then((t) => {
      if (!live) return
      const p = t ? parseAnswer(t.roots) : null
      setParsed(p)
      if (p && !p.writing && state.status === 'ready' && state.t0 && doneIn === null) setDoneIn(Math.max(1, Math.round((Date.now() - state.t0) / 1000)))
    })
    api<LivingStatus>(`/api/doc/${docId}/living`)
      .then((l) => live && setLiving(l))
      .catch(() => live && setLiving(null))
    return () => {
      live = false
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [docId, epoch, dataVersion])
  // the synthesis lands a few seconds after the doc: look again until it does
  useEffect(() => {
    if (!docId || !parsed?.writing) return
    const t = setTimeout(() => {
      loadTree(docId, Date.now()).then((tr) => {
        if (!tr) return
        const p = parseAnswer(tr.roots)
        setParsed(p)
        if (!p.writing && state.status === 'ready' && state.t0) setDoneIn(Math.max(1, Math.round((Date.now() - state.t0) / 1000)))
      })
    }, 2500)
    return () => clearTimeout(t)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [docId, parsed])

  const question =
    state.status === 'idle' ? q : state.status === 'ready' ? parsed?.question || state.question || byId.get(state.docId)?.title || q : state.question
  const nums = parsed ? numberCitations(parsed) : null
  // cited receipts numbered as the prose cites them; an answer that cites
  // nothing (or no answer at all) shows every block it read, numbered
  const cites = !!nums && nums.num.size > 0
  const receipts = parsed && nums ? (cites ? nums.cited : parsed.receipts) : []
  const extra = parsed && nums && cites ? nums.uncited : []
  const [showExtra, setShowExtra] = useState(false)
  const blocksRead = parsed ? parsed.receipts.length : (state.status === 'ready' ? state.sources : null)
  const docsRead = parsed ? new Set(parsed.receipts.map((r) => r.title)).size : state.status === 'ready' ? state.docs : null
  const titleToId = (title: string) => {
    for (const d of byId.values()) if (d.title === title) return d.id
    return null
  }
  const openCite = (blockId: string, title: string) => {
    const id = titleToId(title)
    if (id) onOpenDoc(id, { blockId })
    else notify('That source is not in your docs any more', 'warn')
  }
  const answerDoc = docId ? byId.get(docId) : undefined
  const verified = livingLabel(living)

  const renderRuns = (runs: AnswerRun[]) =>
    runs.map((r, i) =>
      'cite' in r ? (
        <button key={i} type="button" className="lp-cite" title={`${r.title}`} aria-label={`Source ${nums?.num.get(r.cite) ?? ''}: ${r.title}`} onClick={() => openCite(r.cite, r.title)}>
          {nums?.num.get(r.cite) ?? '•'}
        </button>
      ) : r.b ? (
        <b key={i}>{r.t}</b>
      ) : r.i ? (
        <em key={i}>{r.t}</em>
      ) : (
        <span key={i}>{r.t}</span>
      ),
    )

  return (
    <section className={`lp-card lp-ask${wide ? ' wide' : ''}`} aria-labelledby="lp-ask-h">
      <div className="lp-ask-head">
        <span className="lp-ask-bot" aria-hidden>
          <AgentGlyph size={15} />
        </span>
        <h2 id="lp-ask-h">Ask the library</h2>
        {state.status === 'asking' && (
          <span className="lp-ask-status busy">
            <Spinner size={13} label="Asking…" /> Reading your docs…
          </span>
        )}
        {state.status === 'ready' && parsed && !parsed.writing && blocksRead !== null && (
          <span className="lp-ask-status ok">
            <Icon name="applied" size={15} />
            Read {blocksRead} block{blocksRead === 1 ? '' : 's'} in {docsRead} doc{docsRead === 1 ? '' : 's'}
            {doneIn !== null ? ` · ${doneIn} s` : ''}
          </span>
        )}
        {state.status === 'ready' && parsed?.writing && (
          <span className="lp-ask-status busy">
            <Spinner size={13} label="Writing…" /> Writing the answer…
          </span>
        )}
      </div>

      {question ? <p className="lp-ask-q">{/[?.!]$/.test(question) ? question : `${question}?`}</p> : <p className="lp-ask-q muted">Ask your notes a question.</p>}

      {state.status === 'idle' && (
        <div className="lp-ask-invite">
          <p>An answer written only from your own docs. Every claim cites the block it rests on, and the answer is kept under Answers.</p>
          <Button variant="primary" size="sm" icon="ask" kbd="⌘↵" onClick={onAsk} disabled={q.length < MIN_CHARS}>
            Ask
          </Button>
        </div>
      )}
      {state.status === 'asking' && (
        <div className="lp-ask-body">
          <SkeletonLines lines={4} />
          <p className="lp-ask-note muted">Finding the blocks that answer it, then writing from them (up to a minute).</p>
        </div>
      )}
      {state.status === 'none' && (
        <div className="lp-ask-invite">
          <p>Nothing in your docs matches that yet, so there is no answer to write. Try other words.</p>
        </div>
      )}
      {state.status === 'error' && (
        <div className="lp-ask-invite">
          <p className="lp-ask-err">{state.error}</p>
          <Button size="sm" onClick={onAsk}>
            Try again
          </Button>
        </div>
      )}
      {state.status === 'ready' && !parsed && (
        <div className="lp-ask-body">
          <SkeletonLines lines={3} />
        </div>
      )}
      {state.status === 'ready' && parsed && (
        <>
          <div className="lp-ask-body">
            {parsed.writing ? (
              <>
                <Skeleton height={14} width="94%" />
                <Skeleton height={14} width="82%" style={{ marginTop: 10 }} />
                <Skeleton height={14} width="66%" style={{ marginTop: 10 }} />
              </>
            ) : parsed.paras.length > 0 ? (
              parsed.paras.map((p, i) => <p key={i}>{renderRuns(p)}</p>)
            ) : (
              <p className="lp-ask-note">
                {parsed.failed
                  ? 'The answer could not be written this time; the blocks below are what your docs say.'
                  : 'No answer was written here (Claude Code is not installed where Taisce runs); these are the blocks that answer it.'}
              </p>
            )}
          </div>
          {receipts.length > 0 && (
            <div className="lp-rcpts">
              {receipts.map((r, i) => (
                <div key={r.blockId} className="lp-rcpt">
                  <button type="button" className="lp-cite" aria-label={`Open source ${i + 1}`} onClick={() => openCite(r.blockId, r.title)}>
                    {cites ? (nums?.num.get(r.blockId) ?? i + 1) : i + 1}
                  </button>
                  <div>
                    <button type="button" className="lp-rcpt-link" onClick={() => openCite(r.blockId, r.title)}>
                      {r.title || 'Source'}
                    </button>
                    {r.quote && <blockquote>{r.quote}</blockquote>}
                  </div>
                </div>
              ))}
              {extra.length > 0 && (
                <>
                  <button type="button" className="lp-rcpt-more" aria-expanded={showExtra} onClick={() => setShowExtra((s) => !s)}>
                    <Icon name={showExtra ? 'chevronDown' : 'chevronRight'} size={13} />
                    {extra.length} more block{extra.length === 1 ? '' : 's'} read, not cited
                  </button>
                  {showExtra &&
                    extra.map((r) => (
                      <div key={r.blockId} className="lp-rcpt">
                        <span className="lp-cite lp-cite-dim" aria-hidden>
                          ·
                        </span>
                        <div>
                          <button type="button" className="lp-rcpt-link" onClick={() => openCite(r.blockId, r.title)}>
                            {r.title || 'Source'}
                          </button>
                          {r.quote && <blockquote>{r.quote}</blockquote>}
                        </div>
                      </div>
                    ))}
                </>
              )}
            </div>
          )}
          <div className="lp-ask-foot">
            {docId && (
              <span>
                Saved as{' '}
                <Link
                  to={{ kind: 'doc', id: docId }}
                  onClick={(e) => {
                    if (e.metaKey || e.ctrlKey) return
                    e.preventDefault()
                    onOpenDoc(docId)
                  }}
                >
                  Answers › {answerDoc?.title ?? 'this question'}
                </Link>
              </span>
            )}
            {verified && (
              <span className={`lp-ask-verified${living && living.changed > 0 ? ' stale' : ''}`}>
                <Icon name={living && living.changed > 0 ? 'flagged' : 'applied'} size={13} />
                {verified.replace(/^answer · /, '')}
              </span>
            )}
            <Button size="sm" className="lp-ask-follow" onClick={onFollowUp}>
              Ask a follow-up
            </Button>
          </div>
        </>
      )}
    </section>
  )
}
