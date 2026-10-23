// The Trash: deleted docs stay as tombstones (ADR 0001 — nothing is ever hard
// deleted) and this is where they come back from. One row per deleted
// subtree; restore brings back exactly the docs that fell together. Lapis
// page frame (no artboard): the serif title and lead, then one flush card
// of rows in the Library's shape (title + what fell with it, when, Restore).

import { useCallback, useEffect, useMemo, useState } from 'react'
import { api, Doc } from './types'
import { notify, errText } from './Notice'
import { relTime } from './time'
import { Button, Card, EmptyState, Icon, SkeletonLines } from './lapis'
import './screens/places.css'

export interface TrashEntry {
  doc: Doc
  deleted_at: string
  descendants: number
}

/** Restore a deleted doc (and the descendants that fell with it). Shared by
 * the Trash view and the undo toast right after a delete. */
export async function restoreDoc(id: string): Promise<number> {
  const out = await api<{ restored: number }>(`/api/doc/${id}/restore`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: '{}',
  })
  return out.restored
}

export default function Trash({
  dataVersion,
  onOpenDoc,
  onChanged,
}: {
  dataVersion: number
  onOpenDoc: (id: string) => void
  onChanged: () => void
}) {
  const [rows, setRows] = useState<TrashEntry[] | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [filter, setFilter] = useState('')
  const load = useCallback(() => {
    api<TrashEntry[]>('/api/trash')
      .then((r) => setRows(Array.isArray(r) ? r : []))
      .catch((e) => {
        setRows([])
        notify(`could not load the trash: ${errText(e)}`)
      })
  }, [])
  useEffect(load, [load, dataVersion])

  const restore = async (row: TrashEntry) => {
    setBusy(row.doc.id)
    try {
      const n = await restoreDoc(row.doc.id)
      notify(`restored “${row.doc.title}”${n > 1 ? ` and ${n - 1} inside it` : ''} — click to open`, 'ok', {
        onClick: () => onOpenDoc(row.doc.id),
      })
      onChanged()
      load()
    } catch (e) {
      notify(errText(e))
    } finally {
      setBusy(null)
    }
  }

  const shown = useMemo(() => {
    const q = filter.trim().toLowerCase()
    const list = [...(rows ?? [])].sort((a, b) => (a.deleted_at < b.deleted_at ? 1 : -1))
    return q ? list.filter((r) => (r.doc.title || '').toLowerCase().includes(q)) : list
  }, [rows, filter])
  const total = rows?.reduce((n, r) => n + 1 + r.descendants, 0) ?? 0

  return (
    <div className="lp-page lp-place">
      <header className="lp-page-head">
        <div className="lp-page-head-text">
          <div className="lp-page-kicker">
            <span className="lp-pill">
              <Icon name="trash" size={13} />
              Trash
            </span>
            {rows && rows.length > 0 && (
              <span className="muted">
                {total} doc{total === 1 ? '' : 's'} in {rows.length} deletion{rows.length === 1 ? '' : 's'}
              </span>
            )}
          </div>
          <h1 className="lp-page-title">Trash</h1>
          <p className="lp-page-lead">Nothing here is gone for good. Restoring a doc brings back everything that was deleted with it, where it was.</p>
        </div>
      </header>

      {rows && rows.length > 6 && (
        <div className="lp-place-tools">
          <label htmlFor="lp-trash-flt" className="lp-place-sr">
            Filter the Trash
          </label>
          <input id="lp-trash-flt" className="lp-place-filter" placeholder="Filter by title" value={filter} onChange={(e) => setFilter(e.target.value)} />
        </div>
      )}

      {rows === null ? (
        <Card>
          <SkeletonLines lines={4} />
        </Card>
      ) : rows.length === 0 ? (
        <Card>
          <EmptyState icon="trash" title="The Trash is empty" compact>
            Docs you delete from the tree or the Library wait here until you restore them.
          </EmptyState>
        </Card>
      ) : shown.length === 0 ? (
        <Card>
          <EmptyState icon="search" title="Nothing matches" compact>
            Try fewer letters.
          </EmptyState>
        </Card>
      ) : (
        <section className="lp-card lp-card-flush lp-place-table" aria-label="Deleted docs">
          <div className="lp-trash-row lp-place-rowh" aria-hidden>
            <span>Title</span>
            <span>Deleted</span>
            <span />
          </div>
          {shown.map((r) => (
            <div key={r.doc.id} className="lp-trash-row">
              <span className="lp-place-title">
                <Icon name={r.descendants > 0 ? 'folder' : 'doc'} size={15} />
                <span className="lp-place-titletext">
                  <b>{r.doc.title || 'Untitled'}</b>
                  <span className="lp-place-sub">{r.descendants > 0 ? `and ${r.descendants} doc${r.descendants === 1 ? '' : 's'} inside it` : 'one doc'}</span>
                </span>
              </span>
              <span className="muted lp-trash-when" title={new Date(r.deleted_at).toLocaleString()}>
                {relTime(r.deleted_at)}
              </span>
              <span className="lp-trash-act">
                <Button size="sm" icon="history" onClick={() => restore(r)} disabled={busy !== null} aria-label={`Restore ${r.doc.title || 'Untitled'}`}>
                  {busy === r.doc.id ? 'Restoring…' : 'Restore'}
                </Button>
              </span>
            </div>
          ))}
        </section>
      )}
    </div>
  )
}
