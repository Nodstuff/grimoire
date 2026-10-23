// Home (canvas Main; Mobile-Home at phone width): what the stage shows at `/`.
//
// Desktop, top to bottom: the date, the serif greeting and one lead line;
// a row of three cards (Today: the day's to-dos; Needs you: what agents
// changed that waits on a person; Capture + Inbox); "Pick up where you left
// off" (the docs you opened last, as little pages); then Currently reading
// (SERVER mode's EPUB shelf, only when something is on it) beside Since you
// were here (gardener runs and agent-made docs after the last-visit stamp).
//
// Phone: the mark, the date and Settings; the greeting; an "Open or search…"
// field; Today; one "N changes need you" card; Currently reading; then the
// Inbox and Since cards. Capture is the floating button there.
//
// Every region loads on its own and stays quiet (or hidden) when its route
// is missing on an older daemon; nothing here is sample data.

import { useCallback, useEffect, useMemo, useState } from 'react'
import type { OpenDoc } from '../App'
import Link from '../Link'
import { errText, notify } from '../Notice'
import { relTime } from '../time'
import { failureLine, runChips } from '../RunStatus'
import Todo from './TodoList'
import { DAILY_ROOT, clockOf, findDailyDoc, findDailyRoot, localDateTitle, sinceItems, type NewDoc, type SinceItem } from '../briefing'
import { addDays, openItems, type TodoDay } from '../todo'
import { api, type Block, type Doc, type GardenerRun, type QueueRow } from '../types'
import type { Gardener } from '../Gardeners'
import { coverUrl, currentlyReading, type ReadingBook } from '../books'
import { loadRecentIds, recentDocs } from '../omni'
import { targetBlockOf } from '../review'
import { workspaceColor, type Workspace } from '../workspaces'
import { fmtElapsed, parseProgress, runTone } from '../runs'
import { AgentGlyph, Avatar, CountPill, Icon, Kbd, LapisMark, VerdictChip, buttonClass, isAgentName } from '../lapis'
import {
  coverTone,
  dueTodayCount,
  greeting,
  groupTodos,
  leadLine,
  needsRows,
  needsSummary,
  previewText,
  rootOf,
  sinceClock,
  uuidV7Millis,
  whenShort,
  type NeedsRow,
} from '../homeData'
import './home.css'

/* The "since" floor is fixed for the life of the page: the first Home mount
 * reads the previous stamp and posts the new one; later mounts (⌘⇧H again)
 * keep showing the same window instead of emptying it. StrictMode's double
 * effect and re-mounts all share this one promise. */
let visitFloor: Promise<{ since: string | null; supported: boolean }> | null = null

function visitStamp(): Promise<{ since: string | null; supported: boolean }> {
  if (!visitFloor) {
    visitFloor = api<{ last_visit: string | null; previous?: string | null }>('/api/home/visit', { method: 'POST' })
      .then((r) => ({ since: r.previous ?? null, supported: true }))
      .catch(() => ({ since: null, supported: false }))
  }
  return visitFloor
}

/** Test hook: forget the session's stamp. */
export function resetVisitStamp() {
  visitFloor = null
}

const JSON_HEADERS = { 'Content-Type': 'application/json' }
const RECENT_CARDS = 5
const NEEDS_ROWS = 3
const INBOX_ROWS = 6

interface InboxList {
  doc_id: string | null
  items: { id: string; title: string; created_at: string }[]
}

interface FlagRow {
  block: Block
  doc_title: string
  author: string
}

interface Captured {
  doc_id: string
  title: string
}

export default function Home({
  docs,
  workspaces,
  userName,
  onOpenDoc,
  dataVersion,
  onDocsChanged,
  onOpenPalette,
  onMenu,
}: {
  docs: Doc[]
  workspaces: Workspace[]
  /** the person's display name (GET /api/profile); null until known */
  userName: string | null
  onOpenDoc: OpenDoc
  dataVersion: number
  onDocsChanged: () => void
  /** the phone's "Open or search…" field: the ⌘O palette */
  onOpenPalette: () => void
  /** the phone's mark opens the menu drawer (Home has no top bar there) */
  onMenu?: () => void
}) {
  /* ---------- to-do ---------- */
  const todayTitle = localDateTitle()
  const [showYesterday, setShowYesterday] = useState(false)
  const [todoDay, setTodoDay] = useState<TodoDay | null>(null)
  const onTodoLoaded = useCallback((d: TodoDay | null) => {
    // only today's list feeds the header; yesterday is a read-only look
    if (!d || d.date === localDateTitle()) setTodoDay(d)
  }, [])
  const todayDoc = useMemo(() => findDailyDoc(docs, todayTitle), [docs, todayTitle])
  const [logBusy, setLogBusy] = useState(false)
  /** Create today's daily log under Daily (Daily itself when missing) and open it. */
  const startLog = async () => {
    setLogBusy(true)
    try {
      let root = findDailyRoot(docs)
      if (!root) {
        root = await api<Doc>('/api/docs', { method: 'POST', headers: JSON_HEADERS, body: JSON.stringify({ title: DAILY_ROOT, parent_doc_id: null }) })
      }
      const d = await api<Doc>('/api/docs', {
        method: 'POST',
        headers: JSON_HEADERS,
        body: JSON.stringify({ title: todayTitle, parent_doc_id: root.id }),
      })
      onDocsChanged()
      onOpenDoc(d.id)
    } catch (e) {
      notify(errText(e))
    }
    setLogBusy(false)
  }

  /* ---------- needs you (the review queue + audit flags) ---------- */
  const [queue, setQueue] = useState<QueueRow[] | null>(null)
  const [flags, setFlags] = useState<FlagRow[]>([])
  useEffect(() => {
    let live = true
    Promise.all([api<QueueRow[]>('/api/queue').catch(() => null), api<FlagRow[]>('/api/flags').catch(() => [] as FlagRow[])]).then(([q, f]) => {
      if (!live) return
      setQueue(Array.isArray(q) ? q : null)
      setFlags(Array.isArray(f) ? f : [])
    })
    return () => {
      live = false
    }
  }, [dataVersion])

  // who is an agent: the queue and the timeline carry names only
  const [kinds, setKinds] = useState<Map<string, string>>(new Map())
  useEffect(() => {
    api<{ display_name: string; kind: string }[]>('/api/principals')
      .then((ps) => setKinds(new Map((Array.isArray(ps) ? ps : []).map((p) => [p.display_name, p.kind]))))
      .catch(() => {})
  }, [dataVersion])
  const isAgent = useCallback((name: string) => isAgentName(name, kinds.get(name)), [kinds])

  /* ---------- inbox + capture ---------- */
  const [inbox, setInbox] = useState<InboxList | null>(null)
  const [filing, setFiling] = useState(false)
  const loadInbox = useCallback(() => {
    api<InboxList>('/api/inbox')
      .then((r) => setInbox(r && Array.isArray(r.items) ? r : null))
      .catch(() => setInbox(null))
  }, [])
  useEffect(loadInbox, [loadInbox, dataVersion])

  // the gardener list is an admin route (token present in the app; without
  // it the File now button is simply absent)
  const [gardeners, setGardeners] = useState<Map<string, Gardener>>(new Map())
  useEffect(() => {
    api<Gardener[]>('/admin/gardeners')
      .then((gs) => setGardeners(new Map((Array.isArray(gs) ? gs : []).map((g) => [g.id, g]))))
      .catch(() => setGardeners(new Map()))
  }, [dataVersion])
  const filer = useMemo(() => [...gardeners.values()].find((g) => g.kind === 'filer' && g.enabled) ?? null, [gardeners])
  const fileNow = async () => {
    if (!filer) return
    setFiling(true)
    try {
      await api('/admin/garden', { method: 'POST', headers: JSON_HEADERS, body: JSON.stringify({ name: filer.name }) })
      notify(`${filer.name} is filing the Inbox`, 'ok')
    } catch (e) {
      notify(errText(e))
    }
    setFiling(false)
  }

  /* ---------- currently reading (SERVER mode's EPUB shelf; hidden when
     empty or on a daemon without the books routes) ---------- */
  const [reading, setReading] = useState<ReadingBook[] | null>(null)
  useEffect(() => {
    currentlyReading()
      .then((r) => setReading(Array.isArray(r) ? r : null))
      .catch(() => setReading(null))
  }, [dataVersion])

  /* ---------- since you were here ---------- */
  const [since, setSince] = useState<{ since: string | null; supported: boolean } | null>(null)
  const [sinceList, setSinceList] = useState<SinceItem[] | null>(null)
  useEffect(() => {
    let stale = false
    visitStamp().then((v) => {
      if (stale) return
      setSince(v)
      const q = v.since ? `?since=${encodeURIComponent(v.since)}` : ''
      Promise.all([
        api<GardenerRun[]>('/api/runs').catch(() => [] as GardenerRun[]),
        api<{ docs: NewDoc[] }>(`/api/home/since${q}`)
          .then((r) => r.docs ?? [])
          .catch(() => [] as NewDoc[]),
      ]).then(([runs, newDocs]) => {
        if (stale) return
        setSinceList(sinceItems(v.since, Array.isArray(runs) ? runs : [], Array.isArray(newDocs) ? newDocs : []))
      })
    })
    return () => {
      stale = true
    }
  }, [dataVersion])

  /* ---------- header ---------- */
  const open = todoDay ? openItems(todoDay.items) : null
  const groups = todoDay ? groupTodos(todoDay.items) : null
  const reviewTotal = queue ? queue.length + flags.length : null
  const lead = leadLine({
    todos: open ? open.length : null,
    dueToday: todoDay ? dueTodayCount(todoDay.items, todoDay.today) : 0,
    overdue: groups?.overdue.length ?? 0,
    review: reviewTotal,
    inbox: inbox?.items.length ?? null,
    since: since?.since ?? null,
  })
  const hello = greeting(userName)
  const dateLine = new Date().toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long' })

  return (
    <div className="lp-page hm">
      {/* desktop header */}
      <header className="hm-head hm-wide">
        <div className="hm-date">{dateLine}</div>
        <h1 className="hm-hello">{hello}</h1>
        <div className="hm-lead">{lead}</div>
      </header>

      {/* phone header (Mobile-Home) */}
      <header className="hm-phone-head hm-phone">
        <div className="hm-phone-top">
          {onMenu ? (
            <button type="button" className="hm-phone-menu" onClick={onMenu} aria-label="Menu">
              <LapisMark size={26} inset="var(--bg)" />
            </button>
          ) : (
            <LapisMark size={26} inset="var(--bg)" />
          )}
          <span className="hm-date">{dateLine}</span>
          <Link to={{ kind: 'settings', section: 'profile' }} className="hm-phone-settings" aria-label="Settings">
            <Icon name="settings" size={20} />
          </Link>
        </div>
        <h1 className="hm-hello">{hello}</h1>
        <button type="button" className="hm-phone-open" onClick={onOpenPalette}>
          <Icon name="search" size={20} />
          Open or search…
        </button>
      </header>

      <div className="hm-grid hm-grid-3">
        <section className="lp-card lp-card-flush hm-card hm-today" aria-labelledby="hm-h-todo">
          <div className="lp-card-head">
            <Icon name="todo" className="hm-wide" style={{ color: 'var(--accent)' }} />
            <h2 id="hm-h-todo">{showYesterday ? 'Yesterday' : 'Today'}</h2>
            {open && <CountPill n={open.length} className="hm-count" />}
            {!showYesterday && groups && groups.overdue.length > 0 && <span className="hm-phone hm-overdue-word">{groups.overdue.length} overdue</span>}
            <div className="lp-card-actions hm-wide-flex">
              <button type="button" className="hm-link quiet" aria-pressed={showYesterday} onClick={() => setShowYesterday((s) => !s)}>
                {showYesterday ? 'Back to today' : 'Yesterday'}
              </button>
              {todayDoc ? (
                <Link to={{ kind: 'doc', id: todayDoc.id }} className="hm-link">
                  Today’s log
                </Link>
              ) : (
                <button type="button" className="hm-link" disabled={logBusy} onClick={startLog} title={`Creates ${DAILY_ROOT} › ${todayTitle}`}>
                  Start today’s log
                </button>
              )}
            </div>
          </div>
          {showYesterday && <div className="hm-todo-day">{addDays(todayTitle, -1)} · read only</div>}
          <Todo
            key={showYesterday ? 'y' : 't'}
            date={showYesterday ? addDays(todayTitle, -1) : todayTitle}
            readOnly={showYesterday}
            dataVersion={dataVersion}
            onLoaded={onTodoLoaded}
            onOpenDoc={onOpenDoc}
          />
        </section>

        <NeedsCard queue={queue} flags={flags} isAgent={isAgent} />
        <NeedsPhone queue={queue} flags={flags} />

        <section className="lp-card lp-card-flush hm-card hm-capture" aria-labelledby="hm-h-inbox">
          <CaptureBox
            onCaptured={(c) => {
              onDocsChanged()
              loadInbox()
              notify(`Captured to Inbox · ${c.title}`, 'ok', { onClick: () => onOpenDoc(c.doc_id) })
            }}
          />
          <div className="lp-card-head hm-inbox-head">
            <Icon name="inbox" style={{ color: 'var(--ink3)' }} />
            <h2 id="hm-h-inbox">Inbox</h2>
            {inbox && <CountPill n={inbox.items.length} className="hm-count" />}
            <div className="lp-card-actions">
              {filer && inbox && inbox.items.length > 0 && (
                <button type="button" className="hm-link" disabled={filing} onClick={fileNow} title={`Run ${filer.name} now`}>
                  {filing ? 'Filing…' : 'File now'}
                </button>
              )}
              {inbox?.doc_id && (
                <Link to={{ kind: 'doc', id: inbox.doc_id }} className="hm-link">
                  Open
                </Link>
              )}
            </div>
          </div>
          {inbox === null ? (
            <div className="hm-empty">The Inbox needs a newer Taisce.</div>
          ) : inbox.items.length === 0 ? (
            <div className="hm-empty">Nothing waiting. Captures land here until they are filed.</div>
          ) : (
            <>
              {inbox.items.slice(0, INBOX_ROWS).map((it) => (
                <div key={it.id} className="hm-ev hm-inbox-row">
                  <Link to={{ kind: 'doc', id: it.id }} className="hm-inbox-title">
                    {it.title}
                  </Link>
                  <time className="hm-muted" dateTime={it.created_at} title={new Date(it.created_at).toLocaleString()}>
                    {relTime(it.created_at).replace(/ ago$/, '')}
                  </time>
                </div>
              ))}
              {inbox.items.length > INBOX_ROWS && inbox.doc_id && (
                <div className="hm-ev">
                  <Link to={{ kind: 'doc', id: inbox.doc_id }} className="hm-link">
                    {inbox.items.length - INBOX_ROWS} more in the Inbox
                  </Link>
                </div>
              )}
            </>
          )}
        </section>
      </div>

      <RecentDocs docs={docs} workspaces={workspaces} dataVersion={dataVersion} />

      <div className="hm-grid hm-grid-2">
        {reading && reading.length > 0 && <Reading books={reading} />}

        <section className="lp-card lp-card-flush hm-card hm-since" aria-labelledby="hm-h-since">
          <div className="lp-card-head">
            <Icon name="gardeners" style={{ color: 'var(--agent)' }} />
            <h2 id="hm-h-since">Since you were here</h2>
            <div className="lp-card-actions">
              <span className="hm-muted hm-small">{since?.since ? `since ${sinceClock(since.since)}` : since ? 'the last 24 hours' : ''}</span>
            </div>
          </div>
          {sinceList === null ? (
            <div className="hm-empty">…</div>
          ) : sinceList.length === 0 ? (
            <div className="hm-empty">Quiet. No agent has done anything since.</div>
          ) : (
            <ol className="hm-since-list">
              {sinceList.map((it, i) => (
                <SinceRow key={`${it.kind}-${it.at}-${i}`} it={it} isAgent={isAgent} />
              ))}
            </ol>
          )}
        </section>
      </div>
    </div>
  )
}

/* ---------- Needs you ---------- */

function NeedsCard({ queue, flags, isAgent }: { queue: QueueRow[] | null; flags: FlagRow[]; isAgent: (name: string) => boolean }) {
  const rows = useMemo(() => (queue ? needsRows(queue, targetBlockOf) : []), [queue])
  const total = (queue?.length ?? 0) + flags.length
  const shown = rows.slice(0, NEEDS_ROWS)
  const shownOps = shown.reduce((n, r) => n + r.count, 0)
  const rest = (queue?.length ?? 0) - shownOps
  const restWho = new Set(rows.slice(NEEDS_ROWS).map((r) => r.who))
  const more: string[] = []
  if (rest > 0) more.push(`+${rest} more${restWho.size === 1 ? ` from ${[...restWho][0]}` : ''}`)
  if (flags.length) more.push(`${flags.length} audit flag${flags.length === 1 ? '' : 's'}`)
  return (
    <section className="lp-card lp-card-flush hm-card hm-needs hm-wide" aria-labelledby="hm-h-needs">
      <div className="lp-card-head">
        <Icon name="review" style={{ color: 'var(--yellow-mark)' }} />
        <h2 id="hm-h-needs">Needs you</h2>
        {queue && <CountPill n={total} className="hm-count" />}
        <div className="lp-card-actions">
          <Link to={{ kind: 'review' }} className="hm-link">
            Review queue <Kbd>⌘⇧R</Kbd>
          </Link>
        </div>
      </div>
      {queue === null ? (
        <div className="hm-empty">…</div>
      ) : total === 0 ? (
        <div className="hm-empty hm-settled">
          <Icon name="applied" size={15} style={{ color: 'var(--green)' }} />
          Nothing waiting. Every agent change is settled.
        </div>
      ) : (
        <>
          {shown.map((r) => (
            <NeedsLine key={r.key} r={r} agent={isAgent(r.who)} />
          ))}
          <div className="hm-needs-foot">
            <Link to={{ kind: 'review' }} className={buttonClass('primary', 'sm')}>
              Review all {total}
            </Link>
            {more.length > 0 && <span className="hm-muted">{more.join(' · ')}</span>}
          </div>
        </>
      )}
    </section>
  )
}

function NeedsLine({ r, agent }: { r: NeedsRow; agent: boolean }) {
  const to = r.docOp ? { kind: 'review' as const } : r.blockId ? { kind: 'doc' as const, id: r.docId, block: r.blockId } : { kind: 'doc' as const, id: r.docId }
  return (
    <Link to={to} state={r.docOp ? undefined : { review: true }} className="hm-ev hm-ev-link">
      <Avatar name={r.who} agent={agent} size={22} />
      <span className="hm-ev-body">
        <span className={agent ? 'lp-agent-name' : 'hm-person'}>{r.who}</span> {r.before}
        <b>{r.doc}</b>
        {r.after}
        <span className="hm-ev-chip">
          <VerdictChip verdict={r.verdict} />
        </span>
      </span>
      {r.at != null && (
        <time className="hm-muted hm-small" dateTime={new Date(r.at).toISOString()}>
          {clockOf(new Date(r.at).toISOString())}
        </time>
      )}
    </Link>
  )
}

/** The phone's single "N changes need you" card (Mobile-Home). */
function NeedsPhone({ queue, flags }: { queue: QueueRow[] | null; flags: FlagRow[] }) {
  if (!queue) return null
  const total = queue.length + flags.length
  return (
    <Link to={{ kind: 'review' }} className="lp-card hm-needs-phone hm-phone-flex">
      <span className={`hm-needs-phone-mark${total === 0 ? ' calm' : ''}`}>
        <Icon name="review" size={20} />
      </span>
      <span className="hm-needs-phone-text">
        <b>{total === 0 ? 'Nothing needs you' : `${total} change${total === 1 ? '' : 's'} need${total === 1 ? 's' : ''} you`}</b>
        <span>{total === 0 ? 'Every agent change is settled' : needsSummary(queue, flags.length)}</span>
      </span>
      <Icon name="chevronRight" size={20} style={{ color: 'var(--ink3)' }} />
    </Link>
  )
}

/* ---------- Capture ---------- */

function CaptureBox({ onCaptured }: { onCaptured: (c: Captured) => void }) {
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)
  const save = async () => {
    const body = text.trim()
    if (!body || busy) return
    setBusy(true)
    try {
      const c = await api<Captured>('/api/inbox', { method: 'POST', headers: JSON_HEADERS, body: JSON.stringify({ text: body }) })
      setText('')
      onCaptured(c)
    } catch (e) {
      notify(errText(e))
    }
    setBusy(false)
  }
  return (
    <div className="hm-capture-box hm-wide">
      <div className="lp-card-head">
        <Icon name="capture" style={{ color: 'var(--accent)' }} />
        <h2>Capture</h2>
        <div className="lp-card-actions">
          <Kbd>⌘⇧I</Kbd>
        </div>
      </div>
      <div className="hm-capture-field">
        <label htmlFor="hm-capture" className="sr-only">
          Capture a thought
        </label>
        <textarea
          id="hm-capture"
          className="hm-field"
          placeholder="A thought, a link, a list… Enter saves to Inbox"
          value={text}
          disabled={busy}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault()
              save()
            } else if (e.key === 'Escape') {
              e.stopPropagation()
              ;(e.target as HTMLTextAreaElement).blur()
            }
          }}
        />
      </div>
    </div>
  )
}

/* ---------- Pick up where you left off ---------- */

interface CardInfo {
  preview: string
  who: string | null
  kind: string | null
  you: boolean
  at: number | null
}

function RecentDocs({ docs, workspaces, dataVersion }: { docs: Doc[]; workspaces: Workspace[]; dataVersion: number }) {
  const recent = useMemo(() => recentDocs(loadRecentIds(), docs).slice(0, RECENT_CARDS), [docs])
  const ids = recent.map((d) => d.id).join(',')
  const [info, setInfo] = useState<Map<string, CardInfo>>(new Map())
  useEffect(() => {
    let live = true
    for (const id of ids ? ids.split(',') : []) {
      Promise.all([
        api<{ markdown: string }>(`/api/doc/${id}/markdown`).catch(() => null),
        api<{ op: { id: string }; principal_name: string; principal_kind: string; principal_is_yours: boolean }[]>(`/api/doc/${id}/history`).catch(() => null),
      ]).then(([md, hist]) => {
        if (!live) return
        const last = Array.isArray(hist) ? hist[0] : undefined
        setInfo((m) =>
          new Map(m).set(id, {
            preview: md?.markdown ? previewText(md.markdown) : '',
            who: last ? last.principal_name : null,
            kind: last ? last.principal_kind : null,
            you: !!last && last.principal_kind === 'human' && last.principal_is_yours,
            at: last ? uuidV7Millis(last.op.id) : null,
          }),
        )
      })
    }
    return () => {
      live = false
    }
  }, [ids, dataVersion])

  const byId = useMemo(() => new Map(docs.map((d) => [d.id, d])), [docs])
  const wsIndex = useMemo(() => new Map(workspaces.map((w, i) => [w.id, { w, i }])), [workspaces])
  if (recent.length === 0) return null
  return (
    <section className="hm-recent hm-wide" aria-labelledby="hm-h-recent">
      <div className="hm-recent-head">
        <h2 id="hm-h-recent">Pick up where you left off</h2>
        <Link to={{ kind: 'library', workspace: null }} className="hm-link">
          Library
        </Link>
      </div>
      <div className="hm-recent-grid">
        {recent.map((d) => {
          const i = info.get(d.id)
          const ws = d.workspace_id ? wsIndex.get(d.workspace_id) : undefined
          const root = rootOf(d, byId)
          const where = ws ? ws.w.display_name || ws.w.name : root.id === d.id ? 'Unsorted' : root.title
          const dot = ws ? workspaceColor(ws.w, ws.i) : 'var(--line2)'
          return (
            <Link key={d.id} to={{ kind: 'doc', id: d.id }} className="lp-card hm-doccard">
              <span className="hm-doccard-page">
                <b>{d.title || 'Untitled'}</b>
                <span>{i ? i.preview || 'Empty' : ''}</span>
              </span>
              <span className="hm-doccard-meta">
                <span className="hm-doccard-where">
                  <span className="lp-dot" style={{ background: dot }} aria-hidden />
                  {where}
                  {ws?.w.shared ? ' · shared' : ''}
                </span>
                <span>
                  {i?.who ? (
                    <>
                      {i.you ? 'You' : isAgentName(i.who, i.kind) ? <span className="lp-agent-name">{i.who}</span> : i.who}
                      {i.at != null && ` · ${whenShort(i.at)}`}
                    </>
                  ) : (
                    ' '
                  )}
                </span>
              </span>
            </Link>
          )
        })}
      </div>
    </section>
  )
}

/* ---------- Currently reading ---------- */

function Reading({ books }: { books: ReadingBook[] }) {
  return (
    <section className="lp-card lp-card-flush hm-card hm-reading" aria-labelledby="hm-h-read">
      <div className="lp-card-head">
        <Icon name="books" className="hm-wide" style={{ color: 'var(--accent)' }} />
        <h2 id="hm-h-read">Currently reading</h2>
        <div className="lp-card-actions">
          <Link to={{ kind: 'books' }} className="hm-link">
            Books
          </Link>
        </div>
      </div>
      <div className="hm-shelf">
        {books.slice(0, 3).map((b) => {
          const pct = Math.max(0, Math.min(100, Math.round(b.percentage)))
          return (
            <Link key={b.id} to={{ kind: 'read', source: 'book', id: b.id }} className="hm-book" title={b.author ? `${b.title} by ${b.author}` : b.title}>
              <span className={`hm-cover${b.has_cover ? ' art' : ` tone-${coverTone(b.title)}`}`}>
                {b.has_cover ? (
                  <img loading="lazy" src={coverUrl(b.id, b.cover_version)} alt="" />
                ) : (
                  <>
                    <span className="hm-cover-motif" aria-hidden />
                    <span className="hm-cover-title">{b.title}</span>
                    <span className="hm-cover-author">{b.author ?? ''}</span>
                  </>
                )}
              </span>
              <span className="hm-bar" role="progressbar" aria-label={`${b.title} progress`} aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
                <i style={{ width: `${pct}%` }} />
              </span>
              <span className="hm-book-meta">
                <span>{pct}%</span>
                <span className="hm-wide">
                  {b.device}
                  {b.updated_at && ` · ${relTime(b.updated_at).replace(/ ago$/, '')}`}
                </span>
              </span>
            </Link>
          )
        })}
      </div>
    </section>
  )
}

/* ---------- Since you were here ---------- */

function SinceRow({ it, isAgent }: { it: SinceItem; isAgent: (name: string) => boolean }) {
  const time = (
    <time dateTime={it.at} title={new Date(it.at).toLocaleString()}>
      {clockOf(it.at)}
    </time>
  )
  if (it.kind === 'run') {
    const r = it.run
    const tone = runTone(r.status)
    const fail = failureLine(r)
    const chips = runChips(r)
    const p = tone === 'running' ? parseProgress(r.summary) : null
    return (
      <li className="hm-ev">
        {time}
        <span className="lp-avatar lp-avatar-agent hm-bot" aria-hidden>
          <AgentGlyph size={13} />
        </span>
        <span className="hm-ev-body">
          <span className="lp-agent-name">{r.gardener_name}</span>{' '}
          {tone === 'running' ? 'is running' : tone === 'ok' ? 'ran' : tone === 'failed' ? 'failed' : `stopped (${r.status.replace('-', ' ')})`}
          {chips.length > 0 && <span className="hm-muted"> · {chips.map((c) => c.text).join(', ')}</span>}
          {tone === 'running' && (
            <span className="hm-run-progress">
              <span className="hm-bar agent indeterminate">
                <i />
              </span>
              <span className="hm-muted hm-small">
                {p ? `${fmtElapsed(p.seconds)} · ${p.toolCalls} tool call${p.toolCalls === 1 ? '' : 's'}` : 'working…'}
              </span>
            </span>
          )}
          {tone === 'ok' && (
            <span className="hm-ev-chip">
              <span className="lp-verdict lp-verdict-green">
                <Icon name="applied" size={13} strokeWidth={2} />
                Done
              </span>
            </span>
          )}
          {fail && (
            <span className="hm-ev-chip">
              <span className={`lp-verdict ${tone === 'failed' ? 'lp-verdict-red' : 'lp-verdict-yellow'}`}>
                <Icon name={tone === 'failed' ? 'error' : 'warning'} size={13} strokeWidth={2} />
                {tone === 'failed' ? 'Failed' : 'Stopped'}
              </span>
              <span className="hm-muted hm-small hm-fail">{fail}</span>
            </span>
          )}
        </span>
      </li>
    )
  }
  return (
    <li className="hm-ev">
      {time}
      <Avatar name={it.who} agent={isAgent(it.who)} size={22} />
      <span className="hm-ev-body">
        <span className={isAgent(it.who) ? 'lp-agent-name' : 'hm-person'}>{it.who}</span> created{' '}
        <Link to={{ kind: 'doc', id: it.docId }} className="hm-link">
          {it.docTitle}
        </Link>
      </span>
    </li>
  )
}

