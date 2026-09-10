// The briefing home: what the stage shows when no doc is open (also ⌘W and
// ⌘K → Home). Four sections, each collapsible, each loading on its own and
// quietly absent when its route is missing on an older daemon: Review (the
// queue grouped by doc), Since you were here (runs, remote edits, agent-made
// docs after the last-visit stamp), Today (the daily log's Plans as live
// checkboxes + yesterday's leftovers to carry forward) and Stale docs
// (GET /api/freshness, another slice's route). Same typographic register as
// a doc: one centred column, headings, no bordered cards.

import { useCallback, useEffect, useMemo, useState } from 'react'
import type { OpenDoc } from './App'
import { errText, notify } from './Notice'
import { describeChange, isDocOp } from './review'
import { relTime } from './time'
import {
  DAILY_ROOT,
  carryForward,
  dayBefore,
  extractPlans,
  extractSectionLines,
  findDailyDoc,
  findDailyRoot,
  flattenBlocks,
  groupQueueByDoc,
  localDateTitle,
  sinceItems,
  toggleCheckboxLine,
  type NewDoc,
  type PlanGroup,
  type SinceItem,
} from './briefing'
import { api, type ActivityItem, type Doc, type DocTree, type GardenerRun, type QueueRow } from './types'

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
  meta,
  collapsed,
  onToggle,
  children,
}: {
  id: string
  title: string
  meta?: string
  collapsed: boolean
  onToggle: (id: string) => void
  children: React.ReactNode
}) {
  return (
    <section className={`home-section ${collapsed ? 'collapsed' : ''}`}>
      <button className="home-section-head" onClick={() => onToggle(id)} aria-expanded={!collapsed}>
        <span className="home-section-chevron">{collapsed ? '▸' : '▾'}</span>
        <span className="home-section-title">{title}</span>
        {meta && <span className="home-section-meta">{meta}</span>}
      </button>
      {!collapsed && <div className="home-section-body">{children}</div>}
    </section>
  )
}

const JSON_HEADERS = { 'Content-Type': 'application/json' }

export default function Home({
  docs,
  onOpenDoc,
  dataVersion,
  onDocsChanged,
  onQueueChanged,
}: {
  docs: Doc[]
  onOpenDoc: OpenDoc
  dataVersion: number
  onDocsChanged: () => void
  onQueueChanged: () => void
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

  /* ---------- review ---------- */
  const [queue, setQueue] = useState<QueueRow[] | null>(null)
  const [busyDoc, setBusyDoc] = useState<string | null>(null)
  useEffect(() => {
    api<QueueRow[]>('/api/queue')
      .then((q) => setQueue(Array.isArray(q) ? q : []))
      .catch(() => setQueue([]))
  }, [dataVersion])
  const groups = useMemo(() => groupQueueByDoc(queue ?? []), [queue])
  const bulk = async (docId: string, ids: string[], decision: 'accept' | 'decline') => {
    setBusyDoc(docId)
    try {
      const r = await api<{ resolved: number; failed: { error: string }[] }>('/api/resolve_bulk', {
        method: 'POST',
        headers: JSON_HEADERS,
        body: JSON.stringify({ annotation_ids: ids, decision }),
      })
      if (r.failed?.length) notify(`${r.failed.length} could not be ${decision}ed: ${r.failed[0].error}`, 'warn')
      else notify(`${r.resolved} ${decision === 'accept' ? 'accepted' : 'declined'}`, 'ok')
      const q = await api<QueueRow[]>('/api/queue').catch(() => [] as QueueRow[])
      setQueue(q)
      onQueueChanged()
    } catch (e) {
      notify(errText(e))
    }
    setBusyDoc(null)
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
          sinceItems(
            v.since,
            Array.isArray(runs) ? runs : [],
            Array.isArray(activity) ? activity : [],
            Array.isArray(newDocs) ? newDocs : [],
          ),
        )
      })
    })
    return () => {
      stale = true
    }
  }, [dataVersion])

  /* ---------- today ---------- */
  const todayTitle = localDateTitle()
  const yesterdayTitle = dayBefore(todayTitle)
  const todayDoc = useMemo(() => findDailyDoc(docs, todayTitle), [docs, todayTitle])
  const yesterdayDoc = useMemo(() => findDailyDoc(docs, yesterdayTitle), [docs, yesterdayTitle])
  const [todayTree, setTodayTree] = useState<DocTree | null>(null)
  const [yTree, setYTree] = useState<DocTree | null>(null)
  const [todayBusy, setTodayBusy] = useState(false)
  const loadToday = useCallback(() => {
    if (!todayDoc) {
      setTodayTree(null)
      return
    }
    api<DocTree>(`/api/doc/${todayDoc.id}`)
      .then(setTodayTree)
      .catch(() => setTodayTree(null))
  }, [todayDoc])
  useEffect(loadToday, [loadToday, dataVersion])
  useEffect(() => {
    if (!yesterdayDoc) {
      setYTree(null)
      return
    }
    api<DocTree>(`/api/doc/${yesterdayDoc.id}`)
      .then(setYTree)
      .catch(() => setYTree(null))
  }, [yesterdayDoc, dataVersion])

  const todayBlocks = useMemo(() => (todayTree ? flattenBlocks(todayTree.roots) : []), [todayTree])
  const plans = useMemo(() => extractPlans(todayBlocks), [todayBlocks])
  const yBlocks = useMemo(() => (yTree ? flattenBlocks(yTree.roots) : []), [yTree])
  const leftovers = useMemo(() => {
    const questions = extractSectionLines(yBlocks, 'Open Questions')
    const open: PlanGroup[] = extractPlans(yBlocks)
      .map((g) => ({ project: g.project, items: g.items.filter((i) => !i.checked) }))
      .filter((g) => g.items.length > 0)
    const projects = [...new Set([...questions.map((q) => q.project), ...open.map((o) => o.project)])]
    return projects.map((project) => ({
      project,
      questions: questions.find((q) => q.project === project)?.lines ?? [],
      plans: open.find((o) => o.project === project)?.items.map((i) => i.text) ?? [],
    }))
  }, [yBlocks])

  const toggleItem = async (blockId: string, line: number) => {
    if (!todayTree) return
    const block = todayBlocks.find((b) => b.id === blockId)
    if (!block) return
    const content = toggleCheckboxLine(block.content, line)
    if (content === block.content) return
    setTodayBusy(true)
    try {
      await api('/api/propose', {
        method: 'POST',
        headers: JSON_HEADERS,
        body: JSON.stringify({
          doc_id: todayTree.doc.id,
          base_epoch: todayTree.doc.current_epoch,
          ops: [{ kind: { op: 'replace', target: blockId, content } }],
        }),
      })
      const fresh = await api<DocTree>(`/api/doc/${todayTree.doc.id}`)
      setTodayTree(fresh)
    } catch (e) {
      notify(errText(e))
    }
    setTodayBusy(false)
  }

  /** Create today's doc under Daily (Daily itself when missing). Returns the id. */
  const startToday = async (): Promise<string> => {
    let root = findDailyRoot(docs)
    if (!root) {
      root = await api<Doc>('/api/docs', {
        method: 'POST',
        headers: JSON_HEADERS,
        body: JSON.stringify({ title: DAILY_ROOT, parent_doc_id: null }),
      })
    }
    const d = await api<Doc>('/api/docs', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({ title: todayTitle, parent_doc_id: root.id }),
    })
    onDocsChanged()
    return d.id
  }

  const carry = async (project: string, questions: string[], planLines: string[]) => {
    setTodayBusy(true)
    try {
      const id = todayDoc?.id ?? (await startToday())
      const [{ markdown }, tree] = await Promise.all([
        api<{ markdown: string }>(`/api/doc/${id}/markdown`),
        api<DocTree>(`/api/doc/${id}`),
      ])
      let md = markdown
      if (planLines.length) md = carryForward(md, project, 'Plans', planLines, yesterdayTitle)
      if (questions.length) md = carryForward(md, project, 'Open Questions', questions, yesterdayTitle)
      if (md === markdown) {
        notify('already carried forward', 'ok')
      } else {
        await api('/api/propose_markdown', {
          method: 'POST',
          headers: JSON_HEADERS,
          body: JSON.stringify({ doc_id: id, base_epoch: tree.doc.current_epoch, markdown: md }),
        })
        notify(`carried ${planLines.length + questions.length} into ${project}`, 'ok', { onClick: () => onOpenDoc(id) })
      }
      const fresh = await api<DocTree>(`/api/doc/${id}`)
      setTodayTree(fresh)
    } catch (e) {
      notify(errText(e))
    }
    setTodayBusy(false)
  }

  /* ---------- stale docs (another slice's route; hidden on 404) ---------- */
  type StaleRow = { doc_id?: string; id?: string; title?: string; reason?: string; age?: string; last_edit?: string; days?: number }
  const [stale, setStale] = useState<StaleRow[] | null>(null)
  useEffect(() => {
    api<StaleRow[] | { docs: StaleRow[] }>('/api/freshness?limit=8')
      .then((r) => {
        const rows = Array.isArray(r) ? r : Array.isArray(r?.docs) ? r.docs : null
        setStale(rows && rows.length ? rows : null)
      })
      .catch(() => setStale(null))
  }, [dataVersion])

  const reviewCount = queue?.length ?? 0

  return (
    <div className="home-brief">
      <div className="home-brief-head">
        <span className="home-mark">◈</span>
        <span className="home-date">{new Date().toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long' })}</span>
      </div>

      <Section
        id="review"
        title="Review"
        meta={queue === null ? '' : reviewCount ? `${reviewCount} open` : 'nothing waiting'}
        collapsed={collapsed.has('review')}
        onToggle={toggle}
      >
        {queue !== null && groups.length === 0 && <div className="home-empty">the queue is empty ✓</div>}
        {groups.map((g) => (
          <div key={g.docId} className="home-review-doc">
            <div className="home-row">
              <button className="home-link" onClick={() => onOpenDoc(g.docId, { review: true })}>
                {g.title}
              </button>
              <span className="home-dim">
                {g.rows.length} proposal{g.rows.length === 1 ? '' : 's'} · {g.proposers.join(', ')}
              </span>
              <span className="home-actions">
                <button
                  className="home-accept"
                  title="accept all for this doc"
                  disabled={busyDoc !== null}
                  onClick={() => bulk(g.docId, g.annotationIds, 'accept')}
                >
                  ✓
                </button>
                <button
                  className="home-decline"
                  title="decline all for this doc"
                  disabled={busyDoc !== null}
                  onClick={() => bulk(g.docId, g.annotationIds, 'decline')}
                >
                  ✗
                </button>
                <button className="home-link dim" onClick={() => onOpenDoc(g.docId, { review: true })}>
                  review →
                </button>
              </span>
            </div>
            {g.rows
              .filter((r) => isDocOp(r.item.op.kind.op))
              .map((r) => (
                <div key={r.item.annotation.id} className="home-docop">
                  {describeChange(r).headline}
                </div>
              ))}
          </div>
        ))}
      </Section>

      <Section
        id="since"
        title="Since you were here"
        meta={since?.since ? relTime(since.since) : since ? 'last 24h' : ''}
        collapsed={collapsed.has('since')}
        onToggle={toggle}
      >
        {sinceList !== null && sinceList.length === 0 && <div className="home-empty">quiet — nothing happened</div>}
        {(sinceList ?? []).map((it, i) => (
          <div key={`${it.kind}-${it.at}-${i}`} className="home-row">
            <span className={`home-kind ${it.kind}`}>{it.kind === 'run' ? '🌿' : it.kind === 'edit' ? '✎' : '+'}</span>
            {it.docId ? (
              <button className="home-link" onClick={() => onOpenDoc(it.docId!)}>
                {it.text}
              </button>
            ) : (
              <span>{it.text}</span>
            )}
            {it.detail && <span className={`home-dim status-${it.status ?? ''}`}>{it.detail}</span>}
            <span className="home-time">{relTime(it.at)}</span>
          </div>
        ))}
      </Section>

      <Section
        id="today"
        title="Today"
        meta={todayDoc ? todayTitle : 'no log yet'}
        collapsed={collapsed.has('today')}
        onToggle={toggle}
      >
        {!todayDoc && (
          <div className="home-row">
            <button
              className="home-cta"
              disabled={todayBusy}
              onClick={() => {
                setTodayBusy(true)
                startToday()
                  .then((id) => {
                    notify(`started ${todayTitle}`, 'ok', { onClick: () => onOpenDoc(id) })
                  })
                  .catch((e) => notify(errText(e)))
                  .finally(() => setTodayBusy(false))
              }}
            >
              start today’s log
            </button>
            <span className="home-dim">creates {DAILY_ROOT} › {todayTitle}</span>
          </div>
        )}
        {todayDoc && (
          <>
            <div className="home-row">
              <button className="home-link" onClick={() => onOpenDoc(todayDoc.id)}>
                open {todayTitle} →
              </button>
            </div>
            {todayTree && plans.length === 0 && <div className="home-empty">no plans under ### Plans yet</div>}
            {plans.map((g) => (
              <div key={g.project || '—'} className="home-plan-group">
                {g.project && <div className="home-project">{g.project}</div>}
                {g.items.map((it) => (
                  <label key={`${it.blockId}:${it.line}`} className={`home-check ${it.checked ? 'done' : ''}`}>
                    <input
                      type="checkbox"
                      checked={it.checked}
                      disabled={todayBusy}
                      onChange={() => toggleItem(it.blockId, it.line)}
                    />
                    <span>{it.text}</span>
                  </label>
                ))}
              </div>
            ))}
          </>
        )}
        {leftovers.length > 0 && (
          <div className="home-yesterday">
            <div className="home-project dim">from {yesterdayTitle}</div>
            {leftovers.map((l) => (
              <div key={l.project || '—'} className="home-plan-group">
                <div className="home-row">
                  {l.project && <span className="home-project">{l.project}</span>}
                  <button
                    className="home-link dim"
                    disabled={todayBusy}
                    onClick={() => carry(l.project, l.questions, l.plans)}
                  >
                    carry forward →
                  </button>
                </div>
                {l.plans.map((p, i) => (
                  <div key={`p${i}`} className="home-leftover">☐ {p}</div>
                ))}
                {l.questions.map((q, i) => (
                  <div key={`q${i}`} className="home-leftover">? {q}</div>
                ))}
              </div>
            ))}
          </div>
        )}
      </Section>

      {stale && (
        <Section id="stale" title="Stale docs" meta={`${stale.length}`} collapsed={collapsed.has('stale')} onToggle={toggle}>
          {stale.map((s, i) => {
            const id = s.doc_id ?? s.id
            const detail = s.reason ?? s.age ?? (s.days != null ? `${s.days}d` : s.last_edit ? relTime(s.last_edit) : '')
            return (
              <div key={id ?? i} className="home-row">
                {id ? (
                  <button className="home-link" onClick={() => onOpenDoc(id)}>
                    {s.title ?? id}
                  </button>
                ) : (
                  <span>{s.title}</span>
                )}
                {detail && <span className="home-dim">{detail}</span>}
              </div>
            )
          })}
        </Section>
      )}

      <div className="home-hints">
        <span><kbd>⌘K</kbd> search &amp; commands</span>
        <span><kbd>⌘⇧I</kbd> capture</span>
        <span><kbd>⌘T</kbd> tree</span>
        <span><kbd>⌘N</kbd> new doc</span>
        <span><kbd>⌘⇧R</kbd> review</span>
        <span><kbd>?</kbd> all shortcuts</span>
      </div>
    </div>
  )
}
