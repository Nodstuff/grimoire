// The briefing home: what the stage shows when no doc is open (also ⌘W and
// ⌘K → Home). A calm morning page, not a dashboard: the date, one muted
// summary line, then three regions — To-do (the per-day list in the To-do
// doc, todo.rs), Inbox (what quick capture left waiting, when anything is),
// and Since you were here (gardener runs, remote edits, agent-made docs after
// the last-visit stamp, as a compact timeline). Two columns from 1200px: To-do
// and Inbox left, Since right. Review lives in the header chip, staleness in
// the file tree. Every region loads on its own and stays quiet when its route
// is missing on an older daemon.

import { useCallback, useEffect, useMemo, useState } from 'react'
import type { OpenDoc } from './App'
import { errText, notify } from './Notice'
import { relTime } from './time'
import { Chips, ProgressLine, StatusPill, failureLine, runChips } from './RunStatus'
import Todo from './TodoList'
import { DAILY_ROOT, clockOf, findDailyDoc, findDailyRoot, localDateTitle, sinceItems, type NewDoc, type SinceItem } from './briefing'
import { addDays, openItems, summaryLine, type TodoDay } from './todo'
import { api, type ActivityItem, type Doc, type GardenerRun } from './types'
import type { Gardener } from './Gardeners'

const COLLAPSED_KEY = 'grimoire.home.collapsed'

function loadCollapsed(): Set<string> {
  try {
    const raw = localStorage.getItem(COLLAPSED_KEY)
    return new Set(raw ? (JSON.parse(raw) as string[]) : [])
  } catch {
    return new Set()
  }
}

function storeCollapsed(s: Set<string>) {
  try {
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...s]))
  } catch {
    // storage blocked: the state still lives for this session
  }
}

/* The "since" floor is fixed for the life of the page: the first Home mount
 * reads the previous stamp and posts the new one; later mounts (⌘W again)
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

function Section({
  id,
  title,
  count,
  meta,
  collapsed,
  onToggle,
  children,
}: {
  id: string
  title: string
  count?: number | null
  meta?: React.ReactNode
  collapsed: boolean
  onToggle: (id: string) => void
  children: React.ReactNode
}) {
  return (
    <section className={`home-section ${collapsed ? 'collapsed' : ''}`} data-section={id}>
      <div className="home-section-head">
        <button className="home-section-title-btn" onClick={() => onToggle(id)} aria-expanded={!collapsed}>
          <span className="home-section-chevron" aria-hidden>
            {collapsed ? '▸' : '▾'}
          </span>
          <span className="home-section-title">{title}</span>
          {count != null && count > 0 && <span className="home-count">{count}</span>}
        </button>
        {meta && <span className="home-section-meta">{meta}</span>}
      </div>
      {!collapsed && <div className="home-section-body">{children}</div>}
    </section>
  )
}

const JSON_HEADERS = { 'Content-Type': 'application/json' }

interface InboxList {
  doc_id: string | null
  items: { id: string; title: string; created_at: string }[]
}

export default function Home({
  docs,
  onOpenDoc,
  dataVersion,
  onDocsChanged,
}: {
  docs: Doc[]
  onOpenDoc: OpenDoc
  dataVersion: number
  onDocsChanged: () => void
}) {
  const [collapsed, setCollapsed] = useState<Set<string>>(loadCollapsed)
  const toggle = useCallback((id: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      storeCollapsed(next)
      return next
    })
  }, [])

  /* ---------- to-do ---------- */
  const todayTitle = localDateTitle()
  const [showYesterday, setShowYesterday] = useState(false)
  const [todoDay, setTodoDay] = useState<TodoDay | null>(null)
  const onTodoLoaded = useCallback((d: TodoDay | null) => {
    // only today's list feeds the summary; yesterday is a read-only look
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

  /* ---------- inbox ---------- */
  const [inbox, setInbox] = useState<InboxList | null>(null)
  const [filing, setFiling] = useState(false)
  useEffect(() => {
    api<InboxList>('/api/inbox')
      .then((r) => setInbox(r && Array.isArray(r.items) ? r : null))
      .catch(() => setInbox(null))
  }, [dataVersion])
  // the gardener list is an admin route (token present in the app; without
  // it the kind badges and the file-now button are simply absent)
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
        api<ActivityItem[]>('/api/activity?limit=50').catch(() => [] as ActivityItem[]),
        api<{ docs: NewDoc[] }>(`/api/home/since${q}`)
          .then((r) => r.docs ?? [])
          .catch(() => [] as NewDoc[]),
      ]).then(([runs, activity, newDocs]) => {
        if (stale) return
        setSinceList(
          sinceItems(v.since, Array.isArray(runs) ? runs : [], Array.isArray(activity) ? activity : [], Array.isArray(newDocs) ? newDocs : []),
        )
      })
    })
    return () => {
      stale = true
    }
  }, [dataVersion])

  const openCount = todoDay ? openItems(todoDay.items).length : null
  const summary = summaryLine({ todos: openCount, inbox: inbox?.items.length ?? null, since: since?.since ?? null })
  const inboxCount = inbox?.items.length ?? 0

  return (
    <div className="home-brief">
      <header className="home-brief-head">
        <div className="home-brief-title">
          <span className="home-mark" aria-hidden>
            ◈
          </span>
          <span className="home-date">{new Date().toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long' })}</span>
        </div>
        <div className="home-summary">
          {summary}
          {inboxCount > 0 && inbox?.doc_id && (
            <>
              {' · '}
              <button className="home-link dim" onClick={() => onOpenDoc(inbox.doc_id!)}>
                open Inbox →
              </button>
            </>
          )}
        </div>
      </header>

      <div className="home-grid">
        <div className="home-col">
          <Section
            id="todo"
            title="To-do"
            count={openCount}
            meta={
              <>
                <button className={`home-link dim ${showYesterday ? 'on' : ''}`} onClick={() => setShowYesterday((s) => !s)}>
                  {showYesterday ? '← today' : 'yesterday →'}
                </button>
                <span className="home-meta-sep">·</span>
                {todayDoc ? (
                  <button className="home-link dim" onClick={() => onOpenDoc(todayDoc.id)}>
                    today’s log →
                  </button>
                ) : (
                  <button className="home-link dim" disabled={logBusy} onClick={startLog} title={`creates ${DAILY_ROOT} › ${todayTitle}`}>
                    start today’s log
                  </button>
                )}
              </>
            }
            collapsed={collapsed.has('todo')}
            onToggle={toggle}
          >
            {showYesterday && <div className="todo-day-label">{addDays(todayTitle, -1)} · read-only</div>}
            <Todo
              key={showYesterday ? 'y' : 't'}
              date={showYesterday ? addDays(todayTitle, -1) : todayTitle}
              readOnly={showYesterday}
              dataVersion={dataVersion}
              onLoaded={onTodoLoaded}
              onOpenDoc={onOpenDoc}
            />
          </Section>

          {inbox && inboxCount > 0 && (
            <Section
              id="inbox"
              title="Inbox"
              count={inboxCount}
              meta={
                filer ? (
                  <button className="home-link dim" disabled={filing} onClick={fileNow} title={`run ${filer.name} now`}>
                    {filing ? 'filing…' : 'file now →'}
                  </button>
                ) : undefined
              }
              collapsed={collapsed.has('inbox')}
              onToggle={toggle}
            >
              {inbox.items.slice(0, 8).map((it) => (
                <div key={it.id} className="home-row inbox-row">
                  <button className="home-link" onClick={() => onOpenDoc(it.id)}>
                    {it.title}
                  </button>
                  <span className="home-time" title={it.created_at}>
                    {relTime(it.created_at)}
                  </span>
                </div>
              ))}
              {inboxCount > 8 && inbox.doc_id && (
                <button className="home-link dim" onClick={() => onOpenDoc(inbox.doc_id!)}>
                  {inboxCount - 8} more in Inbox →
                </button>
              )}
            </Section>
          )}
        </div>

        <div className="home-col">
          <Section
            id="since"
            title="Since you were here"
            count={sinceList?.length ?? null}
            meta={since?.since ? relTime(since.since) : since ? 'last 24h' : ''}
            collapsed={collapsed.has('since')}
            onToggle={toggle}
          >
            {sinceList !== null && sinceList.length === 0 && <div className="home-empty">quiet — nothing happened</div>}
            <ol className="timeline">
              {(sinceList ?? []).map((it, i) => (
                <TimelineRow key={`${it.kind}-${it.at}-${i}`} it={it} gardeners={gardeners} onOpenDoc={onOpenDoc} />
              ))}
            </ol>
          </Section>
        </div>
      </div>

      <div className="home-hints">
        <span>
          <kbd>⌘K</kbd> search &amp; commands
        </span>
        <span>
          <kbd>⌘⇧I</kbd> capture
        </span>
        <span>
          <kbd>⌘T</kbd> tree
        </span>
        <span>
          <kbd>⌘N</kbd> new doc
        </span>
        <span>
          <kbd>⌘⇧R</kbd> review
        </span>
        <span>
          <kbd>?</kbd> all shortcuts
        </span>
      </div>
    </div>
  )
}

function TimelineRow({ it, gardeners, onOpenDoc }: { it: SinceItem; gardeners: Map<string, Gardener>; onOpenDoc: OpenDoc }) {
  const time = (
    <time className="tl-time" dateTime={it.at} title={new Date(it.at).toLocaleString()}>
      {clockOf(it.at)}
    </time>
  )
  if (it.kind === 'run') {
    const r = it.run
    const kind = gardeners.get(r.gardener)?.kind
    const fail = failureLine(r)
    return (
      <li className={`tl-row run status-${r.status}`}>
        {time}
        <span className="tl-glyph run" aria-hidden>
          🌿
        </span>
        <span className="tl-body">
          <span className="tl-line">
            {kind && <span className={`kind-badge kind-${kind}`}>{kind}</span>}
            <span className="tl-who">{r.gardener_name}</span>
            <StatusPill run={r} />
            <Chips chips={runChips(r)} />
          </span>
          {r.status === 'running' && <ProgressLine run={r} />}
          {fail && <span className="run-failure">{fail}</span>}
        </span>
      </li>
    )
  }
  return (
    <li className={`tl-row ${it.kind}`}>
      {time}
      <span className={`tl-glyph ${it.kind}`} aria-hidden>
        {it.kind === 'edit' ? '✎' : '+'}
      </span>
      <span className="tl-body">
        <span className="tl-line">
          <span className="tl-who">{it.who}</span> {it.kind === 'edit' ? 'edited' : 'created'}{' '}
          <button className="home-link" onClick={() => onOpenDoc(it.docId)}>
            {it.docTitle}
          </button>
        </span>
      </span>
    </li>
  )
}
