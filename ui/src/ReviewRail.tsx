// In-editor review rail: the open items for THIS doc, next to the editor.
// The editor paints the affected blocks (ReviewHighlight); each card here
// explains the change and resolves it. Clicking a card scrolls to its block.

import { useState } from 'react'
import { api, QueueRow } from './types'
import { errText, notify } from './Notice'
import { describeChange, targetBlockOf } from './review'

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

export default function ReviewRail({
  items,
  onChanged,
  onClose,
}: {
  items: QueueRow[]
  /** something was resolved — reload the doc and the review list */
  onChanged: () => void
  onClose: () => void
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

  return (
    <aside className="panel review-rail">
      <div className="panel-head">
        <span>
          {items.length} change{items.length === 1 ? '' : 's'}
        </span>
        <button onClick={onClose}>esc</button>
      </div>
      {items.length > 1 && (
        <div className="rail-bulk">
          <button className="accept" disabled={busy !== null} onClick={() => bulk('accept')}>
            accept all
          </button>
          <button className="bulk-decline" disabled={busy !== null} onClick={() => bulk('decline')}>
            decline all
          </button>
        </div>
      )}
      {items.length === 0 && <div className="palette-empty">nothing to review here ✓</div>}
      {items.map((r) => {
        const d = describeChange(r)
        const yellow = r.item.annotation.kind === 'review'
        const target = targetBlockOf(r)
        const id = r.item.annotation.id
        return (
          <div
            key={id}
            className={`rail-card ${yellow ? 'yellow' : 'red'} ${target ? 'linked' : ''}`}
            onClick={() => {
              if (!target) return
              if (!flashBlock(target)) notify('that block is not in the editor view', 'ok')
            }}
            title={target ? 'click to jump to the block' : undefined}
          >
            <div className="rail-head">
              <span className={`verdict ${yellow ? 'v-yellow' : 'v-red'}`}>{d.badge}</span>
              <span className="card-meta">
                {d.op} · {r.proposer}
                {r.item.op.confidence != null && ` · ${r.item.op.confidence.toFixed(2)}`}
              </span>
            </div>
            <div className="rail-headline">{d.headline}</div>
            {d.before && (
              <div className="rail-col">
                <div className="diff-label">{d.before.label}</div>
                <pre>{d.before.text.slice(0, 600)}</pre>
              </div>
            )}
            {d.after && (
              <div className="rail-col">
                <div className="diff-label">{d.after.label}</div>
                <pre>{d.after.text.slice(0, 600)}</pre>
              </div>
            )}
            {r.item.op.source_refs.length > 0 && (
              <div className="refs">{r.item.op.source_refs.join(' · ')}</div>
            )}
            <div className="actions" onClick={(e) => e.stopPropagation()}>
              <button className="accept" disabled={busy !== null} onClick={() => resolve(id, 'accept')}>
                ✓ accept
              </button>
              <button className="decline" disabled={busy !== null} onClick={() => resolve(id, 'decline')}>
                ✗ decline
              </button>
            </div>
          </div>
        )
      })}
    </aside>
  )
}
