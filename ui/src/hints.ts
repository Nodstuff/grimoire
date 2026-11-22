// User-facing wording for the daemon's precise-but-internal error strings.
// Pure functions, unit-tested; the raw text stays available as a tooltip.

import { ApiError } from './types'

/** Save failures in words a user can act on. The editor keeps the text
 * either way; this only says why it has not landed yet. */
export function saveErrorText(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e)
  if (/fetch|network|ECONN/i.test(raw)) {
    return 'not saved: Taisce is not responding — your edit is kept here and will retry'
  }
  if (/stale base|ahead of doc epoch/i.test(raw)) {
    return 'not saved: this doc changed underneath you — retrying against the new version'
  }
  return `not saved: ${raw} — your edit is kept here and will retry`
}

/** The daemon's refusal classes a retry cannot fix (`api_err`'s `code`):
 * the doc is gone or not yours to write, or the ops are invalid (a base
 * ahead of the doc's epoch included), or the CSRF header was refused.
 * Network failures, 5xx pages and `unauthorized` (sign in, then it lands)
 * stay retryable. */
const PERMANENT = new Set(['not_found', 'forbidden', 'invalid', 'csrf'])
export function isPermanentSaveError(e: unknown): boolean {
  return e instanceof ApiError && e.code != null && PERMANENT.has(e.code)
}

/** A save refused for good: the edit is kept, nothing retries on its own. */
export function permanentSaveErrorText(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e)
  return `not saved: ${raw} — your edit is kept here; it will not retry on its own (edit again or press ⌘↵ to try once more)`
}
