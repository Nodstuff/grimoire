// Relative timestamps ("12s ago", "3h ago") for sync/activity lines. Pure so
// it can be tested with a fixed `now`.

const MIN = 60_000
const HOUR = 60 * MIN
const DAY = 24 * HOUR

/** Human relative time for an ISO timestamp. Returns the raw input when it
 * does not parse; clamps future stamps (clock skew) to "just now". */
export function relTime(iso: string, now: number = Date.now()): string {
  const t = new Date(iso).getTime()
  if (Number.isNaN(t)) return iso
  const ms = now - t
  if (ms < 5_000) return 'just now'
  if (ms < MIN) return `${Math.floor(ms / 1000)}s ago`
  if (ms < HOUR) return `${Math.floor(ms / MIN)}m ago`
  if (ms < DAY) return `${Math.floor(ms / HOUR)}h ago`
  const days = Math.floor(ms / DAY)
  if (days < 30) return `${days}d ago`
  const months = Math.floor(days / 30)
  if (months < 12) return `${months}mo ago`
  return `${Math.floor(days / 365)}y ago`
}
