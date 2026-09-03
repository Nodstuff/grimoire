/** Query-string deep links. The shell (and any host embedding the UI in a
 * frame, e.g. workbox) opens the page as
 * `/?admin_token=…&doc=<uuid>[&block=<uuid>][&tab=<name>]`. `admin_token` is
 * consumed by main.tsx; the rest is parsed here once on boot and scrubbed the
 * same way so a reload does not re-open the doc. `?join=` (grimoire://join
 * links) is handled separately in App. */

export const TABS = ['home', 'review', 'runs', 'graph', 'sharing', 'profile', 'trash'] as const
export type Tab = (typeof TABS)[number]

export interface DeepLink {
  doc?: string
  block?: string
  tab?: Tab
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

/** Parse `?doc=`, `?block=`, `?tab=` from a search string. Ids must be uuids;
 * anything else is dropped (a bad id is not a 404, it is a typo). `block`
 * without `doc` is meaningless and dropped. Returns null if nothing usable. */
export function parseDeepLink(search: string): DeepLink | null {
  const p = new URLSearchParams(search)
  const out: DeepLink = {}
  const doc = p.get('doc')
  if (doc && UUID.test(doc)) {
    out.doc = doc
    const block = p.get('block')
    if (block && UUID.test(block)) out.block = block
  }
  const tab = p.get('tab')
  if (tab && (TABS as readonly string[]).includes(tab)) out.tab = tab as Tab
  return out.doc || out.tab ? out : null
}

/** The search string with the deep-link params removed (others kept). */
export function scrubDeepLink(search: string): string {
  const p = new URLSearchParams(search)
  for (const k of ['doc', 'block', 'tab']) p.delete(k)
  const rest = p.toString()
  return rest ? `?${rest}` : ''
}
