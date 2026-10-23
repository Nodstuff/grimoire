// The review queue (⌘⇧R, /review): open proposals grouped for bulk
// accept/decline, then audit flags. Moved out of App.tsx unchanged for the
// Lapis shell; stage 2 rebuilds it to the Review artboard.

import { useCallback, useEffect, useState } from 'react'
import { notify, errText } from '../Notice'
import { actionLabels, describeChange, isDocOp, targetBlockOf } from '../review'
import { api, Block, QueueRow } from '../types'
import type { OpenDoc } from '../App'

interface FlagRow {
  block: Block
  doc_title: string
  author: string
  target_content: string | null
}

export default function ReviewQueue({
  onChange,
  onOpenDoc,
  dataVersion,
}: {
  onChange: (n: number) => void
  onOpenDoc: OpenDoc
  dataVersion: number
}) {
  const [rows, setRows] = useState<QueueRow[]>([])
  // open the doc with its review rail up, scrolled to the op's block
  const openInDoc = (r: QueueRow) =>
    onOpenDoc(r.item.annotation.doc_id, { review: true, blockId: targetBlockOf(r) ?? undefined })
  const [flags, setFlags] = useState<FlagRow[]>([])
  const [busy, setBusy] = useState<string | null>(null)

  const load = useCallback(() => {
    Promise.all([
      api<QueueRow[]>('/api/queue').catch(() => [] as QueueRow[]),
      api<FlagRow[]>('/api/flags').catch(() => [] as FlagRow[]),
    ]).then(([q, f]) => {
      setRows(q)
      setFlags(f)
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

  // group proposals by proposer for bulk actions
  const byProposer = new Map<string, QueueRow[]>()
  for (const r of rows) {
    if (!byProposer.has(r.proposer)) byProposer.set(r.proposer, [])
    byProposer.get(r.proposer)!.push(r)
  }

  if (rows.length === 0 && flags.length === 0)
    return <div className="empty">review queue is empty ✓</div>

  return (
    <div className="queue">
      <h1 className="queue-title">review</h1>
      {rows.length > 1 &&
        [...byProposer.entries()].map(([who, group]) => (
          <div key={who} className="bulk-bar">
            <span className="who agent">{who}</span>
            <span className="meta">{group.length} proposals</span>
            <span className="gardener-actions">
              <button
                className="accept"
                disabled={busy !== null}
                onClick={() => bulk(group.map((r) => r.item.annotation.id), 'accept')}
              >
                accept all
              </button>
              <button
                className="bulk-decline"
                disabled={busy !== null}
                onClick={() => bulk(group.map((r) => r.item.annotation.id), 'decline')}
              >
                decline all
              </button>
            </span>
          </div>
        ))}
      {rows.map((r) => {
        const op = r.item.op
        const proposed =
          typeof op.kind.content === 'string' ? (op.kind.content as string) : JSON.stringify(op.kind)
        const parked = r.item.annotation.kind === 'parked'
        // doc ops (an agent's rename / move / status / trash through the
        // gate) have no block diff: the card is the sentence describeChange builds
        const docOp = isDocOp(op.kind.op) ? describeChange(r) : null
        const labels = actionLabels(r)
        return (
          <div
            key={r.item.annotation.id}
            className={`card ${parked ? 'red' : 'yellow'} clickable`}
            title="open in doc"
            onClick={() => openInDoc(r)}
          >
            <div className="card-head">
              <span className={`verdict ${parked ? 'v-red' : 'v-yellow'}`}>
                {parked ? 'parked' : 'applied'}
              </span>
              <span className="card-doc">{r.doc_title}</span>
              <span className="card-meta">
                {op.kind.op} · {r.proposer}
                {op.confidence != null && ` · ${op.confidence.toFixed(2)}`}
              </span>
            </div>
            {docOp ? (
              <>
                <div className="rail-headline">{docOp.headline}</div>
                {(docOp.before || docOp.after) && (
                  <div className="diff">
                    {docOp.before && (
                      <div className="diff-col">
                        <div className="diff-label">{docOp.before.label}</div>
                        <pre>{docOp.before.text}</pre>
                      </div>
                    )}
                    {docOp.after && (
                      <div className="diff-col">
                        <div className="diff-label">{docOp.after.label}</div>
                        <pre>{docOp.after.text}</pre>
                      </div>
                    )}
                  </div>
                )}
              </>
            ) : (
              <div className="diff">
                {op.prior && (
                  <div className="diff-col">
                    <div className="diff-label">{parked ? 'current' : 'before'}</div>
                    <pre>
                      {(parked ? r.current_content ?? op.prior.content : op.prior.content).slice(0, 800)}
                    </pre>
                  </div>
                )}
                <div className="diff-col">
                  <div className="diff-label">{parked ? 'proposed' : 'now'}</div>
                  <pre>{proposed.slice(0, 800)}</pre>
                </div>
              </div>
            )}
            {op.source_refs.length > 0 && <div className="refs">{op.source_refs.join(' · ')}</div>}
            <div className="actions" onClick={(e) => e.stopPropagation()}>
              <button
                className="accept"
                disabled={busy === r.item.annotation.id}
                onClick={() => resolve(r.item.annotation.id, 'accept')}
              >
                {labels.accept}
              </button>
              <button
                className="decline"
                disabled={busy === r.item.annotation.id}
                onClick={() => resolve(r.item.annotation.id, 'decline')}
              >
                {labels.decline}
              </button>
              <button className="chip open-in-doc" onClick={() => openInDoc(r)}>
                open in doc →
              </button>
            </div>
          </div>
        )
      })}
      {flags.length > 0 && (
        <>
          <h2 className="runs-title">audit flags</h2>
          {flags.map((f) => (
            <div key={f.block.id} className="card flag">
              <div className="card-head">
                <span className="who agent">{f.author}</span>
                <span className="card-doc" onClick={() => onOpenDoc(f.block.doc_id)}>
                  {f.doc_title}
                </span>
              </div>
              {f.target_content && (
                <div className="thread-target">{f.target_content.split('\n')[0].slice(0, 110)}</div>
              )}
              <div className="flag-text">{f.block.content}</div>
              <div className="actions">
                <button
                  className="decline"
                  disabled={busy === f.block.id}
                  onClick={() => dismiss(f.block.id)}
                >
                  dismiss
                </button>
                <button className="chip" onClick={() => onOpenDoc(f.block.doc_id)}>
                  open doc
                </button>
              </div>
            </div>
          ))}
        </>
      )}
    </div>
  )
}
