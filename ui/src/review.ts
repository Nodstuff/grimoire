// Pure seams for in-editor review: which block a queue item points at, how
// to paint it, and how to describe the change. No DOM, no fetch.

import type { QueueRow } from './types'
import type { ReviewMap, ReviewTone } from './editor/ReviewHighlight'

/** The existing block an item is about, or null for a red insert (the block
 * does not exist yet — it only appears in the rail). */
export function targetBlockOf(item: QueueRow): string | null {
  const { annotation, op } = item.item
  const k = op.kind
  if (k.op === 'insert') {
    // a yellow insert IS applied — the block exists under its block_id
    return annotation.kind === 'review' ? (k.block_id ?? null) : null
  }
  return k.target ?? op.prior?.id ?? null
}

export function toneOf(item: QueueRow): ReviewTone {
  if (item.item.annotation.kind === 'review') return 'yellow'
  return item.item.op.kind.op === 'delete' ? 'red-delete' : 'red'
}

/** blockId → tone. A red delete outranks a red replace on the same block;
 * red (not yet applied) outranks yellow (already applied) so the reviewer
 * sees the pending change first. */
export function buildHighlightMap(items: QueueRow[]): ReviewMap {
  const rank: Record<ReviewTone, number> = { yellow: 0, red: 1, 'red-delete': 2 }
  const map: ReviewMap = {}
  for (const it of items) {
    const id = targetBlockOf(it)
    if (!id) continue
    const tone = toneOf(it)
    const cur = map[id]
    if (!cur || rank[tone] > rank[cur]) map[id] = tone
  }
  return map
}

export interface ChangeSummary {
  /** short badge text */
  badge: 'applied · flagged' | 'proposed · not applied'
  /** 'insert' | 'replace' | 'delete' | 'move' */
  op: string
  /** one-line human description */
  headline: string
  /** left column (what it was / what is live), if any */
  before: { label: string; text: string } | null
  /** right column (what it is now / what is proposed), if any */
  after: { label: string; text: string } | null
}

export function describeChange(item: QueueRow): ChangeSummary {
  const { annotation, op } = item.item
  const k = op.kind
  const yellow = annotation.kind === 'review'
  const badge = yellow ? 'applied · flagged' : 'proposed · not applied'
  const proposed = typeof k.content === 'string' ? k.content : null
  const prior = op.prior?.content ?? null

  if (k.op === 'replace') {
    if (yellow) {
      return {
        badge,
        op: k.op,
        headline: 'replaced this block',
        before: prior != null ? { label: 'was', text: prior } : null,
        after: null, // the live block already shows the new text
      }
    }
    const live = item.current_content ?? prior
    return {
      badge,
      op: k.op,
      headline: 'proposes replacing this block',
      before: live != null ? { label: 'current', text: live } : null,
      after: proposed != null ? { label: 'proposed', text: proposed } : null,
    }
  }
  if (k.op === 'insert') {
    return {
      badge,
      op: k.op,
      headline: yellow ? 'inserted this block' : 'proposes a new block',
      before: null,
      after: proposed != null ? { label: yellow ? 'inserted' : 'proposed', text: proposed } : null,
    }
  }
  if (k.op === 'delete') {
    return {
      badge,
      op: k.op,
      headline: yellow ? 'deleted a block' : 'proposes deleting this block',
      before: yellow && prior != null ? { label: 'was', text: prior } : null,
      after: null,
    }
  }
  if (k.op === 'move') {
    return {
      badge,
      op: k.op,
      headline: yellow ? 'moved this block' : 'proposes moving this block',
      before: null,
      after: null,
    }
  }
  return {
    badge,
    op: k.op,
    headline: `${k.op} ${String(k.target ?? '').slice(0, 8)}`,
    before: prior != null ? { label: yellow ? 'was' : 'current', text: prior } : null,
    after: proposed != null ? { label: yellow ? 'now' : 'proposed', text: proposed } : null,
  }
}
