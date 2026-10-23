/** Query-string deep links. The shell (and any host embedding the UI in a
 * frame, e.g. workbox) opens the page as
 * `/?admin_token=…&doc=<uuid>[&block=<uuid>][&tab=<name>]`. `admin_token` is
 * consumed by main.tsx; the rest is parsed here once on boot and scrubbed the
 * same way so a reload does not re-open the doc. */

export const TABS = ['home', 'review', 'runs', 'graph', 'settings', 'profile', 'trash', 'books', 'ereaders'] as const
export type Tab = (typeof TABS)[number]

/** The sections of the Settings view. */
export type SettingsSection = 'profile' | 'ereaders'

/** Where a `?tab=` lands. `profile` and `ereaders` were views of their own;
 * they are sections of Settings now, and the old links land on them. */
export type TabTarget =
  | { kind: 'home' | 'review' | 'runs' | 'graph' | 'trash' | 'books' }
  | { kind: 'settings'; section: SettingsSection }

export function tabTarget(tab: Tab): TabTarget {
  switch (tab) {
    case 'settings':
    case 'profile':
      return { kind: 'settings', section: 'profile' }
    case 'ereaders':
      return { kind: 'settings', section: 'ereaders' }
    default:
      return { kind: tab }
  }
}

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
