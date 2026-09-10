// Doc freshness: the Stale docs list (⌘K → Stale docs) and the per-doc
// header chip. `verified_at` is set only when an auditor/keeper evaluated the
// doc and found nothing, or a human accepted one of their fixes — never by
// an edit — so "never verified" is the honest default for most docs.

import { useCallback, useEffect, useState } from 'react'
import { api } from './types'
import { notify, errText } from './Notice'
import { relTime } from './time'
import { FreshnessRow, verifiedLabel } from './stale'

export default function Freshness({
  dataVersion,
  onOpenDoc,
}: {
  dataVersion: number
  onOpenDoc: (id: string) => void
}) {
  const [rows, setRows] = useState<FreshnessRow[] | null>(null)
  const [tendedOnly, setTendedOnly] = useState(false)
  const load = useCallback(() => {
    api<FreshnessRow[]>(`/api/freshness?limit=100${tendedOnly ? '&tended=true' : ''}`)
      .then(setRows)
      .catch((e) => {
        setRows([])
        notify(`could not load doc freshness: ${errText(e)}`)
      })
  }, [tendedOnly])
  useEffect(load, [load, dataVersion])

  return (
    <div className="queue">
      <h1 className="queue-title">stale docs</h1>
      <div className="card">
        <div className="card-head">
          <span>never verified first, then the oldest verification</span>
          <label className="toggle meta">
            <input type="checkbox" checked={tendedOnly} onChange={(e) => setTendedOnly(e.target.checked)} />
            tended scopes only
          </label>
        </div>
        {rows === null ? (
          <div className="palette-empty">…</div>
        ) : rows.length === 0 ? (
          <div className="palette-empty">nothing here — every doc with content has been verified</div>
        ) : (
          rows.map((r) => (
            <div key={r.id} className="run freshness-row" onClick={() => onOpenDoc(r.id)} role="button" tabIndex={0}>
              <div className="run-head">
                <span className="who">{r.title}</span>
                <span className={`chip ${r.verified_at ? 'verified' : 'unverified'}`}>{verifiedLabel(r.verified_at)}</span>
                {r.tended && <span className="chip tended">🌿 tended</span>}
              </div>
              <div className="meta">
                {r.path !== r.title ? `${r.path} · ` : ''}edited {relTime(r.last_edited)}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  )
}

/** Header chip: `verified 3d ago` / `never verified` (grey). */
export function FreshnessChip({ docId, dataVersion }: { docId: string; dataVersion: number }) {
  const [verifiedAt, setVerifiedAt] = useState<string | null | undefined>(undefined)
  useEffect(() => {
    let live = true
    api<{ verified_at: string | null }>(`/api/doc/${docId}/freshness`)
      .then((r) => live && setVerifiedAt(r.verified_at ?? null))
      .catch(() => live && setVerifiedAt(undefined))
    return () => {
      live = false
    }
  }, [docId, dataVersion])
  // older daemon without the route: no chip rather than a wrong one
  if (verifiedAt === undefined) return null
  return (
    <span
      className={`chip freshness-chip ${verifiedAt ? 'verified' : 'unverified'}`}
      title={verifiedAt ? `an auditor or keeper checked this doc ${verifiedAt}` : 'no auditor or keeper has evaluated this doc yet'}
    >
      {verifiedLabel(verifiedAt)}
    </span>
  )
}
