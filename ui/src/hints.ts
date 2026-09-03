// User-facing wording for the daemon's precise-but-internal error strings.
// Pure functions, unit-tested; the raw text stays available as a tooltip.

/** A federation failure (pending join / mirror sync `last_error`) in words
 * that say what to do next. Null when the text is not one we recognise. */
export function refusalHint(raw: string | null | undefined): string | null {
  const s = (raw ?? '').toLowerCase()
  if (!s) return null
  if (s.includes('contact is revoked') || s.includes('un-revoke')) return 'the owner has blocked this Mac — only they can unblock it'
  if (s.includes('revoked')) return 'the owner revoked this share — ask for a fresh link'
  if (s.includes('unknown peer')) return 'the owner no longer recognises this Mac — ask for a fresh link'
  if (s.includes('expired') || s.includes('already redeemed') || s.includes('invite')) {
    return 'this link is no longer valid — ask for a fresh one'
  }
  if (s.includes('timed out') || s.includes('unreachable') || s.includes('offline')) {
    return 'the owner’s Mac is offline or unreachable — will keep trying'
  }
  return null
}

/** Save failures in words a user can act on. The editor keeps the text
 * either way; this only says why it has not landed yet. */
export function saveErrorText(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e)
  if (/live session/i.test(raw)) {
    return 'this doc is in a live session — your edit is kept here and will save when it ends'
  }
  if (/fetch|network|ECONN/i.test(raw)) {
    return 'not saved: Grimoire is not responding — your edit is kept here and will retry'
  }
  if (/stale base|ahead of doc epoch/i.test(raw)) {
    return 'not saved: this doc changed underneath you — retrying against the new version'
  }
  return `not saved: ${raw} — your edit is kept here and will retry`
}

/** Propose-mode (shared doc) failures. */
export function proposeErrorText(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e)
  if (/fetch|network|ECONN/i.test(raw)) {
    return 'not sent: Grimoire is not responding — your suggestion is kept here, try again in a moment'
  }
  if (/timed out|unreachable|offline/i.test(raw)) {
    return 'not sent: the owner’s Mac is offline or unreachable — your suggestion is kept here, try again later'
  }
  if (/live session/i.test(raw)) {
    return 'not sent: the owner has this doc in a live session — join it, or try again when it ends'
  }
  if (/revoked|view-only|unknown peer/i.test(raw)) {
    return 'not sent: you no longer have permission to suggest changes to this doc'
  }
  return `not sent: ${raw}`
}
