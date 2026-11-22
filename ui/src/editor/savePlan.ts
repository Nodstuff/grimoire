// Pure decision behind DocEditor's autosave: what to do once a save has
// landed. No React so vitest covers it without a DOM.

export type AfterSave = 'clean' | 'rearm' | 'save-now'

/** After a successful save:
 * - nothing typed during the request → the editor is clean;
 * - typed while mounted → dirty, and the debounce re-arms (more keystrokes
 *   may follow, the retry clock is the backstop);
 * - typed while unmounted (the user switched docs mid-save) → save again
 *   right away: no debounce or retry clock survives the unmount, and those
 *   keystrokes are not in what landed. */
export function afterSave({ editedMeanwhile, mounted }: { editedMeanwhile: boolean; mounted: boolean }): AfterSave {
  if (!editedMeanwhile) return 'clean'
  return mounted ? 'rearm' : 'save-now'
}

/** The idempotency key a save sends (`request_id` on /api/propose): the same
 * request — same doc, base and ops, i.e. a retry — keeps its key, so if an
 * earlier attempt landed but its answer was lost the daemon answers with
 * that outcome instead of applying the ops twice. Anything else gets a new
 * one. */
export interface PendingRequest {
  body: string
  id: string
}
export function requestFor(prev: PendingRequest | null, body: string, mint: () => string = () => crypto.randomUUID()): PendingRequest {
  return prev && prev.body === body ? prev : { body, id: mint() }
}
