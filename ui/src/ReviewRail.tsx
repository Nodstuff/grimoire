// The doc inspector's Review tab: the open items for THIS doc. The editor
// paints the affected blocks (ReviewHighlight) with their own Keep / Revert
// bar; each card here explains the change and resolves it too. Clicking a
// card scrolls to its block.

import { useState } from 'react'
import { api, QueueRow } from './types'
import { errText, notify } from './Notice'
import { actionLabels, describeChange, targetBlockOf } from './review'
import { Avatar, Button, EmptyState, VerdictChip, isAgentName } from './lapis'

/** Scroll a rendered block into view and flash it (same treatment as a
 * [[Doc#^uuid]] anchor). Returns false when the block is not in the DOM. */
export function flashBlock(blockId: string): boolean {
  const el = document.querySelector(`[data-block-id="${blockId}"]`)
  if (!el) return false
  el.scrollIntoView({ block: 'center', behavior: 'smooth' })
  el.classList.remove('anchor-flash')
  // re-trigger the animation even if it is mid-flight
  void (el as HTMLElement).offsetWidth
  el.classList.add('anchor-flash')
  setTimeout(() => el.classList.remove('anchor-flash'), 1600)
  return true
}

function cap(s: string) {
  return s ? s[0].toUpperCase() + s.slice(1) : s
}

export default function ReviewRail({
  items,
  onChanged,
  canResolve = true,
}: {
  items: QueueRow[]
  /** something was resolved — reload the doc and the review list */
  onChanged: () => void
  onClose?: () => void
  /** a viewer of a shared workspace sees the items, not the buttons */
  canResolve?: boolean
}) {
  const [busy, setBusy] = useState<string | null>(null)

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
    onChanged()
  }

  const bulk = async (decision: 'accept' | 'decline') => {
    const ids = items.map((r) => r.item.annotation.id)
    if (ids.length === 0) return
    setBusy('bulk')
    try {
      await api('/api/resolve_bulk', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ annotation_ids: ids, decision }),
      })
    } catch (e) {
      notify(errText(e))
    }
    setBusy(null)
    onChanged()
  }

  if (items.length === 0)
    return (
      <div className="ins-empty">
        <EmptyState icon="applied" title="Nothing to review">
          Every change to this doc is settled.
        </EmptyState>
      </div>
    )

  return (
    <div className="rv-list">
      {items.length > 1 && canResolve && (
        <div className="rv-bulk">
          <span>
            {items.length} changes waiting
          </span>
          <Button size="sm" variant="ok" disabled={busy !== null} onClick={() => bulk('accept')}>
            Keep all
          </Button>
          <Button size="sm" disabled={busy !== null} onClick={() => bulk('decline')}>
            Revert all
          </Button>
        </div>
      )}
      {items.map((r) => {
        const d = describeChange(r)
        const yellow = r.item.annotation.kind === 'review'
        const target = targetBlockOf(r)
        const id = r.item.annotation.id
        const labels = actionLabels(r)
        return (
          <div
            key={id}
            className={`rv-card ${yellow ? 'yellow' : 'red'}${target ? ' linked' : ''}`}
            onClick={() => {
              if (!target) return
              if (!flashBlock(target)) notify('that block is not in the editor view', 'ok')
            }}
            title={target ? 'Show the block' : undefined}
          >
            <div className="rv-card-head">
              <VerdictChip verdict={yellow ? 'flagged' : 'parked'} />
              <span className="rv-card-op">{d.op.replace('_', ' ')}</span>
            </div>
            <div className="rv-card-who">
              <Avatar name={r.proposer} agent={isAgentName(r.proposer)} showName />
              {r.item.op.confidence != null && <span className="rv-card-meta">· confidence {r.item.op.confidence.toFixed(2)}</span>}
            </div>
            <div className="rv-card-headline">{cap(d.headline)}</div>
            {d.before && (
              <div className="rv-card-col">
                <div className="rv-card-label">{d.before.label}</div>
                <pre className={yellow ? 'was' : ''}>{d.before.text.slice(0, 600)}</pre>
              </div>
            )}
            {d.after && (
              <div className="rv-card-col">
                <div className="rv-card-label">{d.after.label}</div>
                <pre className="now">{d.after.text.slice(0, 600)}</pre>
              </div>
            )}
            {r.item.op.source_refs.length > 0 && (
              <div className="rv-card-refs">from {r.item.op.source_refs.join(' · ')}</div>
            )}
            {canResolve && (
              <div className="rv-card-actions" onClick={(e) => e.stopPropagation()}>
                <Button size="sm" variant="ok" disabled={busy !== null} onClick={() => resolve(id, 'accept')}>
                  {cap(labels.accept)}
                </Button>
                <Button size="sm" disabled={busy !== null} onClick={() => resolve(id, 'decline')}>
                  {cap(labels.decline)}
                </Button>
              </div>
            )}
          </div>
        )
      })}
    </div>
  )
}
