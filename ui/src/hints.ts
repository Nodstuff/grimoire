// User-facing wording for the daemon's precise-but-internal error strings.
// Pure functions, unit-tested; the raw text stays available as a tooltip.

/** Save failures in words a user can act on. The editor keeps the text
 * either way; this only says why it has not landed yet. */
export function saveErrorText(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e)
  if (/fetch|network|ECONN/i.test(raw)) {
    return 'not saved: Grimoire is not responding — your edit is kept here and will retry'
  }
  if (/stale base|ahead of doc epoch/i.test(raw)) {
    return 'not saved: this doc changed underneath you — retrying against the new version'
  }
  return `not saved: ${raw} — your edit is kept here and will retry`
}
