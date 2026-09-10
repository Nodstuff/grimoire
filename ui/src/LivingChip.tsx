// Living answers: an ask-the-vault answer doc says whether the blocks it
// cites still stand. `answer · verified 2h ago` while they do; `answer · N
// sources changed — refreshing` once any moved on (the 16:00 sweep re-grounds
// it as reviewable yellows). Renders nothing for docs that are not answers.

import { useEffect, useState } from 'react'
import { api } from './types'
import { LivingStatus, livingLabel, livingTone } from './stale'

export default function LivingChip({ docId, dataVersion }: { docId: string; dataVersion: number }) {
  const [status, setStatus] = useState<LivingStatus | null>(null)
  useEffect(() => {
    let live = true
    api<LivingStatus>(`/api/doc/${docId}/living`)
      .then((s) => live && setStatus(s))
      .catch(() => live && setStatus(null))
    return () => {
      live = false
    }
  }, [docId, dataVersion])
  const label = livingLabel(status)
  if (!label || !status) return null
  const changed = status.sources.filter((s) => s.changed)
  const title =
    changed.length > 0
      ? `changed since this answer was written: ${changed
          .map((s) => `${s.doc_title ?? 'a doc'}${s.gone ? ' (block removed)' : ''}`)
          .join(', ')} — the next sweep proposes a refreshed answer for review`
      : `${status.sources.length} cited block${status.sources.length === 1 ? '' : 's'} unchanged since the answer was written`
  return (
    <span className={`chip living-chip living-${livingTone(status)}`} title={title}>
      {label}
    </span>
  )
}
