// The review queue (⌘⇧R, /review), built to the canvas Review artboard:
// pending agent changes grouped by doc (or by agent) with word diffs, doc-op
// items (rename / move / status, a parked trash), audit flags, a By agent card
// with bulk Keep all / Revert all (flagged only: a parked change needs its own
// yes), and keys J/K (select), A (keep/apply), D (revert/decline), O (open),
// ⇧A (keep all from the selected agent).

import { Fragment, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { notify, errText } from '../Notice'
import { describeChange, isDocOp, targetBlockOf } from '../review'
import { api, Block, Doc, QueueRow } from '../types'
import type { OpenDoc } from '../App'
import type { Workspace } from '../workspaces'
import { workspaceColor } from '../workspaces'
import { Avatar, Button, CountPill, EmptyState, Icon, Kbd, Spinner, TextArea, VerdictChip } from '../lapis'
import {
  bulkWord,
  captions,
  docLocation,
  groupByAgent,
  groupByDoc,
  idOf,
  isParked,
  isReorder,
  sentenceOf,
  sideOf,
  uuidTime,
  whenLabel,
  wordDiff,
  type DiffPart,
} from './reviewModel'
import './review.css'

interface FlagRow {
  block: Block
  doc_title: string
  author: string
  target_content: string | null
}

type View = 'doc' | 'agent' | 'flags'
const VIEWS: { id: View; label: string }[] = [
  { id: 'doc', label: 'By doc' },
  { id: 'agent', label: 'By agent' },
  { id: 'flags', label: 'Audit flags' },
]

/** a selectable row: a queue item or an audit flag */
type Sel = { kind: 'item'; id: string; row: QueueRow } | { kind: 'flag'; id: string; flag: FlagRow }

const MAX_TEXT = 800

export default function ReviewQueue({
  onChange,
  onOpenDoc,
  dataVersion,
  docs = [],
  workspaces = [],
}: {
  onChange: (n: number) => void
  onOpenDoc: OpenDoc
  dataVersion: number
  docs?: Doc[]
  workspaces?: Workspace[]
}) {
  const [rows, setRows] = useState<QueueRow[]>([])
  const [flags, setFlags] = useState<FlagRow[]>([])
  const [loaded, setLoaded] = useState(false)
  const [busy, setBusy] = useState<string | null>(null)
  const [view, setView] = useState<View>('doc')
  const [selId, setSelId] = useState<string | null>(null)
  // the inline comment box under one item (POST /api/comment on its block)
  const [commentFor, setCommentFor] = useState<string | null>(null)
  const [commentText, setCommentText] = useState('')
  const listRef = useRef<HTMLDivElement>(null)

  const byId = useMemo(() => new Map(docs.map((d) => [d.id, d])), [docs])
  const wsColor = useMemo(() => new Map(workspaces.map((w, i) => [w.id, workspaceColor(w, i)])), [workspaces])

  // open the doc with its review rail up, scrolled to the op's block
  const openInDoc = (r: QueueRow) =>
    onOpenDoc(r.item.annotation.doc_id, { review: true, blockId: targetBlockOf(r) ?? undefined })

  const load = useCallback(() => {
    Promise.all([
      api<QueueRow[]>('/api/queue').catch(() => [] as QueueRow[]),
      api<FlagRow[]>('/api/flags').catch(() => [] as FlagRow[]),
    ]).then(([q, f]) => {
      setRows(q)
      setFlags(f)
      setLoaded(true)
      onChange(q.length + f.length)
    })
  }, [onChange])

  useEffect(load, [load, dataVersion])

  const resolve = async (annotationId: string, decision: 'accept' | 'decline') => {
    setBusy(annotationId)
    try {
      await api('/api/resolve', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ annotation_id: annotationId, decision }),
      })
    } catch (e) {
      notify(errText(e))
    }
    setBusy(null)
    load()
  }

  const dismiss = async (commentId: string) => {
    setBusy(commentId)
    await api('/api/flags/dismiss', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ comment_id: commentId }),
    }).catch((e) => notify(String(e)))
    setBusy(null)
    load()
  }

  const bulk = async (ids: string[], decision: 'accept' | 'decline') => {
    if (ids.length === 0) return
    setBusy('bulk')
    await api('/api/resolve_bulk', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ annotation_ids: ids, decision }),
    }).catch((e) => notify(String(e)))
    setBusy(null)
    load()
  }

  const postComment = async (blockId: string) => {
    const text = commentText.trim()
    if (!text) return
    setBusy('comment')
    try {
      await api('/api/comment', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ block_id: blockId, text }),
      })
      setCommentFor(null)
      setCommentText('')
      notify('Comment added', 'ok')
    } catch (e) {
      notify(errText(e))
    }
    setBusy(null)
  }

  const docGroups = useMemo(() => groupByDoc(rows), [rows])
  const agentGroups = useMemo(() => groupByAgent(rows), [rows])
  const flaggedN = rows.filter((r) => !isParked(r)).length
  const parkedN = rows.length - flaggedN

  // the rows in on-screen order, for J/K
  const order: Sel[] = useMemo(() => {
    const items: Sel[] = []
    const add = (r: QueueRow) => items.push({ kind: 'item', id: idOf(r), row: r })
    if (view === 'doc') docGroups.forEach((g) => g.rows.forEach(add))
    if (view === 'agent') agentGroups.forEach((g) => g.rows.forEach(add))
    if (view !== 'agent' || rows.length === 0) flags.forEach((f) => items.push({ kind: 'flag', id: f.block.id, flag: f }))
    return items
  }, [view, docGroups, agentGroups, flags, rows.length])

  // keep a selection on screen: the first row, or the next one after a resolve
  const lastIdx = useRef(0)
  useEffect(() => {
    const i = order.findIndex((s) => s.id === selId)
    if (i >= 0) {
      lastIdx.current = i
      return
    }
    setSelId(order.length ? order[Math.min(lastIdx.current, order.length - 1)].id : null)
  }, [order, selId])
  const sel = order.find((s) => s.id === selId) ?? null

  const move = (d: 1 | -1) => {
    if (!order.length) return
    const i = order.findIndex((s) => s.id === selId)
    const n = Math.max(0, Math.min(order.length - 1, (i < 0 ? 0 : i) + d))
    setSelId(order[n].id)
    requestAnimationFrame(() =>
      listRef.current?.querySelector<HTMLElement>(`[data-sel="${order[n].id}"]`)?.scrollIntoView({ block: 'nearest' }),
    )
  }

  // keys: J/K/A/D/O and ⇧A, never while typing or with a palette/dialog open
  const keyRef = useRef<(e: KeyboardEvent) => void>(() => {})
  keyRef.current = (e: KeyboardEvent) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return
    const t = e.target as HTMLElement | null
    if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return
    if (document.querySelector('[role="dialog"], [aria-modal="true"]')) return
    const k = e.key.toLowerCase()
    if (e.shiftKey && k !== 'a') return
    if (k === 'j') return e.preventDefault(), move(1)
    if (k === 'k') return e.preventDefault(), move(-1)
    if (!sel || busy) return
    if (k === 'a' && e.shiftKey && sel.kind === 'item') {
      const g = agentGroups.find((x) => x.agent === sel.row.proposer)
      if (g && g.flagged.length) {
        e.preventDefault()
        bulk(g.flagged.map(idOf), 'accept')
      }
      return
    }
    if (sel.kind === 'item') {
      if (k === 'a') return e.preventDefault(), void resolve(sel.id, 'accept')
      if (k === 'd') return e.preventDefault(), void resolve(sel.id, 'decline')
      if (k === 'o') return e.preventDefault(), openInDoc(sel.row)
    } else {
      if (k === 'd') return e.preventDefault(), void dismiss(sel.id)
      if (k === 'o') return e.preventDefault(), onOpenDoc(sel.flag.block.doc_id)
    }
  }
  useEffect(() => {
    const h = (e: KeyboardEvent) => keyRef.current(e)
    window.addEventListener('keydown', h)
    return () => window.removeEventListener('keydown', h)
  }, [])

  const now = Date.now()
  const when = (id: string) => {
    const t = uuidTime(id)
    return t == null ? null : whenLabel(t, now)
  }

  const header = (
    <div className="rv-head">
      <div className="rv-head-text">
        <h1 className="rv-title">Review</h1>
        <div className="rv-summary">
          {flaggedN > 0 && (
            <span className="lp-verdict lp-verdict-yellow">
              <Icon name="flagged" size={13} strokeWidth={2} />
              {flaggedN} flagged · applied
            </span>
          )}
          {parkedN > 0 && (
            <span className="lp-verdict lp-verdict-red">
              <Icon name="parked" size={13} strokeWidth={2} />
              {parkedN} parked · not applied
            </span>
          )}
          {flags.length > 0 && (
            <span className="rv-muted rv-sm">
              {rows.length > 0 && '· '}
              {flags.length} audit flag{flags.length === 1 ? '' : 's'}
            </span>
          )}
          {loaded && rows.length === 0 && flags.length === 0 && <span className="rv-muted rv-sm">Nothing waiting for you</span>}
        </div>
      </div>
      {(rows.length > 0 || flags.length > 0) && (
        <div className="rv-seg" role="group" aria-label="Group by">
          {VIEWS.map((v) => (
            <button key={v.id} type="button" className={view === v.id ? 'on' : ''} aria-pressed={view === v.id} onClick={() => setView(v.id)}>
              {v.label}
            </button>
          ))}
        </div>
      )}
    </div>
  )

  if (!loaded)
    return (
      <div className="rv-page">
        {header}
        <Spinner label="Loading the review queue…" />
      </div>
    )

  if (rows.length === 0 && flags.length === 0)
    return (
      <div className="rv-page">
        {header}
        <EmptyState icon="review" title="Review queue is empty">
          Agent changes that need a person land here: flagged ones already applied, parked ones waiting for your yes.
        </EmptyState>
      </div>
    )

  // ---------- one item ----------
  const item = (r: QueueRow, showDocName: boolean) => {
    const id = idOf(r)
    const k = r.item.op.kind
    const op = r.item.op
    const parked = isParked(r)
    const s = sentenceOf(r)
    const cap = captions(r)
    const selected = selId === id
    const t = when(op.id)
    // a comment needs a live block: not a doc op, not a parked insert
    const commentBlock = isDocOp(k.op) ? null : targetBlockOf(r)
    const policy = byId.get(r.item.annotation.doc_id)?.review_policy
    const meta = [t, op.confidence != null ? `confidence ${op.confidence.toFixed(2)}` : null, policy ? `doc has a ${policy} policy` : null]
      .filter(Boolean)
      .join(' · ')
    return (
      <div
        key={id}
        data-sel={id}
        className={`rv-item${selected ? ' sel' : ''}`}
        onClick={(e) => {
          if ((e.target as HTMLElement).closest('button, a')) return
          setSelId(id)
        }}
      >
        <div className="rv-ih">
          <VerdictChip verdict={parked ? 'parked' : 'flagged'} short />
          <Avatar name={r.proposer} agent showName />
          <span>
            {s.pre}
            {s.strong && <b>{s.strong}</b>}
            {s.post}
            {showDocName && (
              <>
                {' '}
                in <b>{r.doc_title}</b>
              </>
            )}
          </span>
          {meta && (
            <span className="rv-meta" title={op.source_refs.length ? `Sources: ${op.source_refs.join(' · ')}` : undefined}>
              {meta}
            </span>
          )}
        </div>
        {body(r)}
        <div className="rv-acts">
          <Button
            size="sm"
            variant={k.op === 'delete_doc' ? 'secondary' : 'ok'}
            className={k.op === 'delete_doc' ? 'rv-btn-trash' : undefined}
            disabled={busy !== null}
            onClick={() => resolve(id, 'accept')}
          >
            {cap.accept}
            {selected && <Kbd>A</Kbd>}
          </Button>
          <Button
            size="sm"
            variant={k.op === 'delete_doc' ? 'ok' : 'secondary'}
            disabled={busy !== null}
            onClick={() => resolve(id, 'decline')}
          >
            {cap.decline}
            {selected && <Kbd>D</Kbd>}
          </Button>
          {commentBlock && commentFor !== id && (
            <Button size="sm" variant="ghost" onClick={() => (setCommentFor(id), setCommentText(''))}>
              Comment
            </Button>
          )}
          {showDocName && (
            <Button size="sm" variant="ghost" onClick={() => openInDoc(r)}>
              Open in doc
              {selected && <Kbd>O</Kbd>}
            </Button>
          )}
          {selected && <span className="rv-note">{cap.note}</span>}
        </div>
        {commentBlock && commentFor === id && (
          <form
            className="rv-comment"
            onSubmit={(e) => {
              e.preventDefault()
              postComment(commentBlock)
            }}
          >
            <TextArea
              label={`Comment on this change by ${r.proposer}`}
              hideLabel
              autoFocus
              rows={2}
              value={commentText}
              placeholder="Say what should change, or why…"
              onChange={(e) => setCommentText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                  e.preventDefault()
                  postComment(commentBlock)
                }
                if (e.key === 'Escape') setCommentFor(null)
              }}
            />
            <div className="rv-acts">
              <Button size="sm" variant="primary" type="submit" disabled={busy !== null || !commentText.trim()} kbd="⌘↵">
                Comment
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setCommentFor(null)}>
                Cancel
              </Button>
            </div>
          </form>
        )}
      </div>
    )
  }

  // ---------- an item's body: a word diff, a rename line, a trash box ----------
  const body = (r: QueueRow): ReactNode => {
    const k = r.item.op.kind
    const op = r.item.op
    const parked = isParked(r)
    const clip = (s: string | null | undefined) => (s ?? '').slice(0, MAX_TEXT)
    if (k.op === 'delete_doc') {
      const n = typeof k.doc_count === 'number' && k.doc_count > 0 ? k.doc_count : 1
      return (
        <div className="rv-trash">
          <Icon name="trash" style={{ color: 'var(--red)' }} />
          <span>
            <b>
              {n} doc{n === 1 ? '' : 's'}
            </b>{' '}
            would go to Trash, restorable from there.
          </span>
        </div>
      )
    }
    if (k.op === 'rename_doc' || (k.op === 'move_doc' && !isReorder(r))) {
      const d = describeChange(r)
      if (!d.before || !d.after) return null
      return (
        <div className="rv-rename">
          <s>{d.before.text}</s>
          <ArrowRight />
          <span>{d.after.text}</span>
          {k.op === 'move_doc' && <span className="rv-rename-note">· {parked ? 'proposed parent' : 'new parent'}</span>}
        </div>
      )
    }
    if (isDocOp(k.op)) return null // set_status: the sentence says it all
    const proposed = typeof k.content === 'string' ? k.content : null
    if (k.op === 'replace') {
      const left = parked ? (r.current_content ?? op.prior?.content ?? '') : (op.prior?.content ?? '')
      const right = proposed ?? ''
      const parts = wordDiff(clip(left), clip(right))
      return (
        <div className="rv-diff">
          <DiffSide label={parked ? 'Current' : 'Was'} parts={sideOf(parts, 'old')} />
          <DiffSide label={parked ? 'Proposed' : 'Now'} parts={sideOf(parts, 'new')} />
        </div>
      )
    }
    if (k.op === 'insert')
      return (
        <div className="rv-diff">
          <DiffSide label={parked ? 'Would add' : 'Added'} parts={[{ kind: 'ins', text: clip(proposed) }]} />
        </div>
      )
    if (k.op === 'delete') {
      const text = clip(r.current_content ?? op.prior?.content)
      if (!text) return null
      return (
        <div className="rv-diff">
          <DiffSide label={parked ? 'Would remove' : 'Removed'} parts={[{ kind: 'del', text }]} />
        </div>
      )
    }
    if (k.op === 'move') {
      const text = clip(r.current_content ?? op.prior?.content)
      return text ? (
        <div className="rv-diff">
          <DiffSide label={parked ? 'Would move' : 'Moved'} parts={[{ kind: 'same', text }]} />
        </div>
      ) : null
    }
    return proposed ? (
      <div className="rv-diff">
        <DiffSide label={parked ? 'Proposed' : 'Now'} parts={[{ kind: 'same', text: clip(proposed) }]} />
      </div>
    ) : null
  }

  // ---------- group cards ----------
  const docGroupCard = (g: (typeof docGroups)[number], i: number) => {
    const doc = byId.get(g.docId)
    const hasKids = docs.some((d) => d.parent_id === g.docId)
    const loc = docLocation(g.docId, byId, workspaces)
    const color = doc?.workspace_id ? wsColor.get(doc.workspace_id) : undefined
    const flagged = g.rows.filter((r) => !isParked(r))
    const hid = `rv-g-${i}`
    return (
      <section key={g.docId} className="lp-card rv-grp" aria-labelledby={hid}>
        <div className="rv-gh">
          <Icon name={hasKids ? 'folder' : 'doc'} size={15} style={{ color: 'var(--ink3)' }} />
          <h2 id={hid}>{g.title}</h2>
          {(loc.workspace || loc.path) && (
            <span className="rv-muted rv-sm rv-loc">
              {color && <span className="lp-dot" style={{ background: color }} aria-hidden />}
              {[loc.workspace, loc.path].filter(Boolean).join(' › ')}
            </span>
          )}
          <CountPill n={g.rows.length} />
          <span className="rv-acts rv-gh-acts">
            {flagged.length > 1 && (
              <>
                <Button size="sm" variant="ok" disabled={busy !== null} onClick={() => bulk(flagged.map(idOf), 'accept')}>
                  {bulkWord('Keep', flagged.length)}
                </Button>
                <Button size="sm" disabled={busy !== null} onClick={() => bulk(flagged.map(idOf), 'decline')}>
                  {bulkWord('Revert', flagged.length)}
                </Button>
              </>
            )}
            <Button size="sm" variant="ghost" onClick={() => openInDoc(g.rows[0])}>
              Open in doc
            </Button>
          </span>
        </div>
        {g.rows.map((r) => item(r, false))}
      </section>
    )
  }

  const agentGroupCard = (g: (typeof agentGroups)[number], i: number) => {
    const hid = `rv-a-${i}`
    return (
      <section key={g.agent} className="lp-card rv-grp" aria-labelledby={hid}>
        <div className="rv-gh">
          <Avatar name={g.agent} agent />
          <h2 id={hid} className="rv-gh-agent">
            {g.agent}
          </h2>
          <CountPill n={g.rows.length} />
          {g.flagged.length > 0 && (
            <span className="rv-acts rv-gh-acts">
              <Button size="sm" variant="ok" disabled={busy !== null} onClick={() => bulk(g.flagged.map(idOf), 'accept')}>
                Keep all
              </Button>
              <Button size="sm" disabled={busy !== null} onClick={() => bulk(g.flagged.map(idOf), 'decline')}>
                Revert all
              </Button>
            </span>
          )}
        </div>
        {g.rows.map((r) => item(r, true))}
      </section>
    )
  }

  const flagsCard = flags.length > 0 && (
    <section className="lp-card rv-grp" aria-labelledby="rv-flags">
      <div className="rv-gh">
        <Icon name="comment" size={15} style={{ color: 'var(--agent)' }} />
        <h2 id="rv-flags">Audit flags</h2>
        <CountPill n={flags.length} />
      </div>
      {flags.map((f) => {
        const id = f.block.id
        const selected = selId === id
        const t = when(id)
        return (
          <div
            key={id}
            data-sel={id}
            className={`rv-item${selected ? ' sel' : ''}`}
            onClick={(e) => {
              if ((e.target as HTMLElement).closest('button, a')) return
              setSelId(id)
            }}
          >
            <div className="rv-ih">
              <Avatar name={f.author} agent showName />
              <span>
                on{' '}
                <a
                  href={`/doc/${f.block.doc_id}`}
                  className="rv-doclink"
                  onClick={(e) => {
                    if (e.metaKey || e.ctrlKey) return
                    e.preventDefault()
                    onOpenDoc(f.block.doc_id)
                  }}
                >
                  {f.doc_title}
                </a>
              </span>
              {t && <span className="rv-meta">{t}</span>}
            </div>
            {f.target_content && <div className="rv-flag-target">{f.target_content.split('\n')[0].slice(0, 110)}</div>}
            <div className="rv-flag-text">{f.block.content}</div>
            <div className="rv-acts">
              <Button size="sm" onClick={() => onOpenDoc(f.block.doc_id)}>
                Open in doc
                {selected && <Kbd>O</Kbd>}
              </Button>
              <Button size="sm" variant="ghost" disabled={busy !== null} onClick={() => dismiss(id)}>
                Dismiss
                {selected && <Kbd>D</Kbd>}
              </Button>
            </div>
          </div>
        )
      })}
    </section>
  )

  let main: ReactNode
  if (view === 'flags')
    main = flagsCard || (
      <EmptyState icon="comment" title="No audit flags">
        Agents flag a doc here when its text disagrees with what they find.
      </EmptyState>
    )
  else if (rows.length === 0) main = flagsCard
  else
    main = (
      <>
        {view === 'doc' ? docGroups.map(docGroupCard) : agentGroups.map(agentGroupCard)}
        {view === 'doc' && flagsCard}
      </>
    )

  return (
    <div className="rv-page">
      {header}
      <div className="rv-cols">
        <div className="rv-main" ref={listRef}>
          {main}
        </div>
        <aside className="rv-aside">
          {agentGroups.length > 0 && (
            <section className="lp-card rv-agents" aria-labelledby="rv-by-agent">
              <h2 id="rv-by-agent">By agent</h2>
              {agentGroups.map((g) => (
                <div key={g.agent} className="rv-agent-row">
                  <Avatar name={g.agent} agent showName />
                  <CountPill n={g.rows.length} />
                  {g.flagged.length > 0 && (
                    <span className="rv-agent-acts">
                      <Button size="sm" variant="ok" disabled={busy !== null} onClick={() => bulk(g.flagged.map(idOf), 'accept')}>
                        Keep all
                      </Button>
                      <Button size="sm" disabled={busy !== null} onClick={() => bulk(g.flagged.map(idOf), 'decline')}>
                        Revert all
                      </Button>
                    </span>
                  )}
                </div>
              ))}
              <p className="rv-agents-note">Parked changes are never included in “Keep all”: each needs its own yes.</p>
            </section>
          )}
          <section className="lp-card rv-keys" aria-labelledby="rv-keys">
            <h2 id="rv-keys">Keys</h2>
            <div className="rv-keys-grid">
              {(
                [
                  [['J', 'K'], 'Next, previous change'],
                  [['A'], 'Keep or apply'],
                  [['D'], 'Revert or decline'],
                  [['O'], 'Open in doc'],
                  [['⇧A'], 'Keep all from this agent'],
                ] as const
              ).map(([keys, what]) => (
                <Fragment key={what}>
                  <span>
                    {keys.map((k, i) => (
                      <Fragment key={k}>
                        {i > 0 && ' '}
                        <Kbd>{k}</Kbd>
                      </Fragment>
                    ))}
                  </span>
                  <span>{what}</span>
                </Fragment>
              ))}
            </div>
          </section>
        </aside>
      </div>
    </div>
  )
}

function DiffSide({ label, parts }: { label: string; parts: DiffPart[] }) {
  return (
    <div className="rv-dside">
      <div className="rv-lab">{label}</div>
      <p>
        {parts.map((p, i) =>
          p.kind === 'del' ? <del key={i}>{p.text}</del> : p.kind === 'ins' ? <ins key={i}>{p.text}</ins> : <Fragment key={i}>{p.text}</Fragment>,
        )}
      </p>
    </div>
  )
}

function ArrowRight() {
  return (
    <svg className="rv-arrow" viewBox="0 0 24 24" width="18" height="18" aria-label="to" role="img">
      <path d="M5 12h14m-5-5 5 5-5 5" />
    </svg>
  )
}
