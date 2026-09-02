// The Trash: deleted docs stay as tombstones (ADR 0001 — nothing is ever hard
// deleted) and this is where they come back from. One row per deleted
// subtree; restore brings back exactly the docs that fell together.

import { useCallback, useEffect, useState } from 'react'
import { api, Doc } from './types'
import { notify, errText } from './Notice'
import { relTime } from './time'

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
  const load = useCallback(() => {
    api<TrashEntry[]>('/api/trash')
      .then(setRows)
      .catch((e) => {
        setRows([])
        notify(`could not load the trash: ${errText(e)}`)
      })
  }, [])
  useEffect(load, [load, dataVersion])

  const restore = async (row: TrashEntry) => {
    try {
      const n = await restoreDoc(row.doc.id)
      notify(`restored “${row.doc.title}”${n > 1 ? ` and ${n - 1} inside it` : ''}`, 'ok', {
        onClick: () => onOpenDoc(row.doc.id),
      })
      onChanged()
      load()
    } catch (e) {
      notify(errText(e))
    }
  }

  return (
    <div className="queue">
      <h1 className="queue-title">trash</h1>
      <div className="card">
        <div className="card-head">
          <span>deleted docs</span>
          <span className="meta">restoring brings back everything that was deleted with the doc</span>
        </div>
        {rows === null ? (
          <div className="palette-empty">…</div>
        ) : rows.length === 0 ? (
          <div className="palette-empty">nothing in the trash</div>
        ) : (
          rows.map((r) => (
            <div key={r.doc.id} className="profile-kv trash-row">
              <span className="trash-title">{r.doc.title || '(untitled)'}</span>
              <span className="meta">
                {r.descendants > 0 ? `+${r.descendants} inside · ` : ''}
                deleted {relTime(r.deleted_at)}
              </span>
              <button className="chip" onClick={() => restore(r)}>
                restore
              </button>
            </div>
          ))
        )}
      </div>
    </div>
  )
}
