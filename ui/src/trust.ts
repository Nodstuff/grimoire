// Share trust tiers (federation): what happens to a grantee's edits.

import type { ShareTrust } from './types'

export interface TrustTier {
  value: ShareTrust
  label: string
  hint: string
}

export const TRUST_TIERS: TrustTier[] = [
  { value: 'review', label: 'review', hint: 'their edits wait for you' },
  { value: 'yellow', label: 'trusted', hint: 'their edits apply, flagged for review' },
  { value: 'green', label: 'maintainer', hint: "their edits apply directly; you're notified" },
]

export function trustLabel(t: ShareTrust | string | null | undefined): string {
  return TRUST_TIERS.find((x) => x.value === t)?.label ?? 'review'
}

export function trustHint(t: ShareTrust | string | null | undefined): string {
  return TRUST_TIERS.find((x) => x.value === t)?.hint ?? TRUST_TIERS[0].hint
}
