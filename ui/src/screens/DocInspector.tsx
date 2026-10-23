// The doc inspector (Lapis Doc artboard): one card at the right of the doc
// with tabs Review / Comments / History / Share, replacing the old floating
// side panels. On a wide window it sits beside the doc; narrower, it is a
// sheet (from the right, or from the bottom on a phone). Tending, which has
// no tab of its own, opens here from the doc's head.

import { useMemo, useRef, useState, type ReactNode } from 'react'
import { api, Block, HistoryRow, QueueRow } from '../types'
import { errText, notify } from '../Notice'
import { Avatar, Button, CountPill, EmptyState, Icon, IconButton, VerdictChip } from '../lapis'
import ReviewRail from '../ReviewRail'
import SharePanel from '../SharePanel'
import TendPanel from '../TendPanel'
import { versions, type Version } from '../editor/docMeta'
import type { Doc } from '../types'

export type InspectorTab = 'review' | 'comments' | 'history' | 'share' | 'tend'

export interface Principal {
  name: string
  agent: boolean
  yours: boolean
}

export function DocInspector({
  tab,
  setTab,
  onClose,
  doc,
  reviewItems,
  afterResolve,
  comments,
  allBlocks,
  commentTarget,
  setCommentTarget,
  focusThread,
  onPosted,
  history,
  who,
  signedIn,
  readOnly,
  dataVersion,
  closeLabel = 'Hide inspector',
}: {
  tab: InspectorTab
  setTab: (t: InspectorTab) => void
  onClose: () => void
  doc: Doc
  reviewItems: QueueRow[]
  afterResolve: () => void
  comments: Block[]
  allBlocks: Block[]
  commentTarget: string | null
  setCommentTarget: (id: string | null) => void
  /** a block whose thread to bring forward (its comment bubble was clicked) */
  focusThread: string | null
  onPosted: () => void
  history: HistoryRow[]
  who: (principalId: string) => Principal
  signedIn: boolean
  readOnly: boolean
  dataVersion: number
  closeLabel?: string
}) {
  const roots = comments.filter((c) => !c.parent_id || !comments.some((p) => p.id === c.parent_id))
  const tabs: { id: InspectorTab; label: string; extra?: ReactNode }[] = [
    {
      id: 'review',
      label: 'Review',
      extra: reviewItems.length > 0 ? <CountPill n={reviewItems.length} tone="yellow" label={`${reviewItems.length} to review`} /> : undefined,
    },
    { id: 'comments', label: 'Comments', extra: <CountPill n={roots.length} /> },
    { id: 'history', label: 'History' },
    ...(signedIn ? [{ id: 'share' as const, label: 'Share' }] : []),
  ]
  const refs = useRef<(HTMLButtonElement | null)[]>([])
  const onKey = (e: React.KeyboardEvent, i: number) => {
    const d = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0
    if (!d) return
    e.preventDefault()
    const n = (i + d + tabs.length) % tabs.length
    refs.current[n]?.focus()
    setTab(tabs[n].id)
  }

  return (
    <div className="ins">
      <div className="ins-tabs" role="tablist" aria-label="Doc inspector">
        {tabs.map((t, i) => (
          <button
            key={t.id}
            ref={(el) => {
              refs.current[i] = el
            }}
            type="button"
            role="tab"
            id={`ins-tab-${t.id}`}
            aria-selected={tab === t.id}
            aria-controls="ins-panel"
            tabIndex={tab === t.id ? 0 : -1}
            data-autofocus={tab === t.id ? '' : undefined}
            className={tab === t.id ? 'on' : ''}
            onClick={() => setTab(t.id)}
            onKeyDown={(e) => onKey(e, i)}
          >
            {t.label}
            {t.extra}
          </button>
        ))}
        <IconButton icon="close" label={closeLabel} size="sm" className="ins-close" onClick={onClose} />
      </div>
      <div className="ins-body" id="ins-panel" role="tabpanel" aria-labelledby={tab === 'tend' ? undefined : `ins-tab-${tab}`} aria-label={tab === 'tend' ? 'Tending' : undefined}>
        {tab === 'review' && <ReviewRail items={reviewItems} onChanged={afterResolve} canResolve={!readOnly} />}
        {tab === 'comments' && (
          <CommentsPane
            comments={comments}
            allBlocks={allBlocks}
            target={commentTarget}
            setTarget={setCommentTarget}
            focusThread={focusThread}
            onPosted={onPosted}
            who={who}
            readOnly={readOnly}
          />
        )}
        {tab === 'history' && <HistoryPane rows={history} />}
        {tab === 'share' && signedIn && <SharePanel docId={doc.id} title={doc.title} />}
        {tab === 'tend' && (
          <>
            <div className="ins-sec">
              <b>Tending</b>
              <span>agents that look after this doc</span>
            </div>
            <TendPanel doc={doc} dataVersion={dataVersion} />
          </>
        )}
      </div>
    </div>
  )
}

/* ---------- comments ---------- */

/** A block's first line as plain words: no heading or list marks, no
 * emphasis or code ticks, wikilinks as their names. */
export function plainLine(md: string): string {
  return md
    .split('\n')[0]
    .replace(/^#+\s*/, '')
    .replace(/^\s*([-*+]|\d+\.)\s+/, '')
    .replace(/\[\[([^\]|]+)\|([^\]]+)\]\]/g, '$2')
    .replace(/\[\[([^\]]+)\]\]/g, '$1')
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/(\*\*|__|\*|_|`)/g, '')
    .trim()
}

function CommentsPane({
  comments,
  allBlocks,
  target,
  setTarget,
  focusThread,
  onPosted,
  who,
  readOnly,
}: {
  comments: Block[]
  allBlocks: Block[]
  target: string | null
  setTarget: (t: string | null) => void
  focusThread: string | null
  onPosted: () => void
  who: (principalId: string) => Principal
  readOnly: boolean
}) {
  const byId = useMemo(() => new Map(allBlocks.map((b) => [b.id, b])), [allBlocks])
  const isComment = (id: string | null) => !!id && byId.get(id)?.block_type === 'comment'
  const roots = comments.filter((c) => !isComment(c.parent_id))
  const repliesOf = (id: string) => comments.filter((c) => c.parent_id === id)
  // the clicked bubble's threads first
  const ordered = focusThread
    ? [...roots.filter((r) => r.refers_to === focusThread), ...roots.filter((r) => r.refers_to !== focusThread)]
    : roots

  const snippet = (id: string | null) => {
    const c = id ? (byId.get(id)?.content ?? '') : ''
    const line = plainLine(c)
    return line.length > 90 ? line.slice(0, 90) + '…' : line
  }

  return (
    <div className="cm-list">
      {target && !readOnly && (
        <div className="thr thr-new">
          <div className="thr-q">{snippet(target)}</div>
          <Composer
            blockId={target}
            replyTo={null}
            placeholder="Comment…  ⌘↵ posts"
            onDone={() => {
              setTarget(null)
              onPosted()
            }}
            onCancel={() => setTarget(null)}
          />
        </div>
      )}
      {ordered.map((c) => (
        <Thread
          key={c.id}
          root={c}
          replies={repliesOf(c.id)}
          quote={snippet(c.refers_to)}
          focused={!!focusThread && c.refers_to === focusThread}
          who={who}
          readOnly={readOnly}
          onPosted={onPosted}
        />
      ))}
      {roots.length === 0 && !target && (
        <div className="ins-empty">
          <EmptyState icon="comment" title="No comments yet">
            {readOnly ? 'Nobody has commented on this doc.' : 'Select some text in the doc and choose Comment to start a thread.'}
          </EmptyState>
        </div>
      )}
    </div>
  )
}

function CommentRow({ c, who }: { c: Block; who: (id: string) => Principal }) {
  const p = who(c.created_by)
  return (
    <div className="cm">
      <Avatar name={p.name} agent={p.agent} />
      <div className="cm-b">
        <div className="cm-h">
          {p.agent ? <span className="lp-agent-name">{p.name}</span> : <b>{p.yours ? 'You' : p.name}</b>}
          <span className="cm-when">{p.agent ? 'flag · ' : ''}v{c.epoch}</span>
        </div>
        <div className="cm-text">{c.content}</div>
      </div>
    </div>
  )
}

function Thread({
  root,
  replies,
  quote,
  focused,
  who,
  readOnly,
  onPosted,
}: {
  root: Block
  replies: Block[]
  quote: string
  focused: boolean
  who: (id: string) => Principal
  readOnly: boolean
  onPosted: () => void
}) {
  const [replying, setReplying] = useState(false)
  const [busy, setBusy] = useState(false)
  const agentFlag = who(root.created_by).agent
  const dismiss = async () => {
    setBusy(true)
    try {
      await api('/api/flags/dismiss', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ comment_id: root.id }),
      })
      onPosted()
    } catch (e) {
      notify(errText(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <div className={`thr${focused ? ' focused' : ''}`}>
      {quote && <div className="thr-q">{quote}</div>}
      <CommentRow c={root} who={who} />
      {replies.map((r) => (
        <div key={r.id} className="thr-reply">
          <CommentRow c={r} who={who} />
        </div>
      ))}
      {!readOnly &&
        (replying ? (
          <div className="thr-reply">
            <Composer
              blockId={root.refers_to}
              replyTo={root.id}
              placeholder="Reply…  ⌘↵ posts"
              onDone={() => {
                setReplying(false)
                onPosted()
              }}
              onCancel={() => setReplying(false)}
            />
          </div>
        ) : (
          <div className="thr-acts">
            <Button size="sm" onClick={() => setReplying(true)}>
              Reply
            </Button>
            {agentFlag && (
              <Button size="sm" variant="ghost" disabled={busy} onClick={dismiss}>
                Dismiss flag
              </Button>
            )}
          </div>
        ))}
    </div>
  )
}

function Composer({
  blockId,
  replyTo,
  placeholder,
  onDone,
  onCancel,
}: {
  blockId: string | null
  replyTo: string | null
  placeholder: string
  onDone: () => void
  onCancel: () => void
}) {
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)
  const post = async () => {
    if (!text.trim() || !blockId || busy) return
    setBusy(true)
    try {
      await api('/api/comment', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ block_id: blockId, text: text.trim(), reply_to: replyTo }),
      })
      setText('')
      onDone()
    } catch (e) {
      notify(errText(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <div className="cm-compose">
      <label>
        <span className="dv-sr">{replyTo ? 'Reply' : 'Comment'}</span>
        <textarea
          autoFocus
          value={text}
          placeholder={placeholder}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
              e.preventDefault()
              post()
            }
            if (e.key === 'Escape') {
              e.stopPropagation()
              onCancel()
            }
          }}
        />
      </label>
      <div className="cm-compose-acts">
        <Button size="sm" variant="primary" disabled={!text.trim() || busy} onClick={post}>
          Post
        </Button>
        <Button size="sm" variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  )
}

/* ---------- history ---------- */

function opText(r: HistoryRow): { before: string | null; after: string | null } {
  const k = r.op.kind
  const after = typeof k.content === 'string' ? (k.content as string) : null
  const before = r.op.prior?.content ?? null
  if (k.op === 'insert') return { before: null, after }
  if (k.op === 'delete') return { before, after: null }
  return { before, after }
}

function VersionRow({ v }: { v: Version }) {
  const [open, setOpen] = useState(false)
  const verdict = v.verdict === 'yellow' ? 'flagged' : v.verdict === 'red' ? 'parked' : null
  return (
    <div className={`hist${open ? ' open' : ''}`}>
      <div className="hist-row">
        <Avatar name={v.who} agent={v.agent} />
        <span className="hist-what">
          {v.agent ? <span className="lp-agent-name">{v.who}</span> : <b>{v.yours ? 'You' : v.who}</b>} {v.summary}{' '}
          {verdict && <VerdictChip verdict={verdict} short className="hist-v" />}
          <span className="hist-when">{v.epoch < 0 ? 'not applied' : `v${v.epoch}`}</span>
        </span>
        <Button size="sm" variant="ghost" className="hist-diff" aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? 'Hide' : 'Diff'}
        </Button>
      </div>
      {open && (
        <div className="hist-ops">
          {v.rows.map((r) => {
            const { before, after } = opText(r)
            return (
              <div key={r.op.id} className="hist-op">
                <span className="hist-op-kind">{r.op.kind.op.replace('_', ' ')}</span>
                {before && <pre className="was">{before.slice(0, 400)}</pre>}
                {after && <pre className="now">{after.slice(0, 400)}</pre>}
                {!before && !after && <span className="hist-op-none">no text change</span>}
                {r.op.source_refs.length > 0 && <div className="hist-refs">from {r.op.source_refs.join(' · ')}</div>}
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}

function HistoryPane({ rows }: { rows: HistoryRow[] }) {
  const list = useMemo(() => versions(rows), [rows])
  if (rows.length === 0)
    return (
      <div className="ins-empty">
        <EmptyState icon="history" title="No history yet">
          Every save and every agent change lands here, newest first.
        </EmptyState>
      </div>
    )
  return (
    <div className="hist-list">
      <div className="ins-sec">
        <b>Recent versions</b>
        <span>
          <Icon name="history" size={13} /> {list.length}
        </span>
      </div>
      {list.map((v) => (
        <VersionRow key={v.epoch} v={v} />
      ))}
    </div>
  )
}
