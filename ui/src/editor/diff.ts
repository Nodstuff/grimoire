// Editor → gate diff (the heart of the 5.1 spike): compare the editor's
// top-level entries against the loaded baseline and emit block ops.
// Structure rules mirror the importer: nesting is derived from heading
// levels; order keys are per-sibling-list base-36 fractions.

export interface BaselineBlock {
  id: string
  /** round-tripped markdown (parse→serialize), the comparison form */
  content: string
  parent: string | null
  order_key: string
}

export interface Entry {
  id: string | null
  content: string
  /** heading level when the entry IS a heading, else 0 */
  level: number
}

export interface Op {
  kind: Record<string, unknown> & { op: string }
  source_refs: string[]
}

const DIGITS = '0123456789abcdefghijklmnopqrstuvwxyz'
const MID = DIGITS[18] // 'i'

/** A base-36 fractional key strictly between `a` and `b` (null = open end).
 * Total: when the bounds are inverted or equal (`a >= b`) — a broken sibling
 * list, or two docs handed the same key — the answer is a key strictly
 * after `a` (`a` + a mid digit) rather than an infinite loop. Never ends in
 * '0', so every key has room below it. Mirrors crates/store/src/order_key.rs. */
export function keyBetween(a: string | null, b: string | null): string {
  if (a != null && b != null && a >= b) return a + MID
  const av = a ?? ''
  let out = ''
  let i = 0
  for (;;) {
    const da = i < av.length ? Math.max(0, DIGITS.indexOf(av[i])) : 0
    const db = b == null ? 36 : i < b.length ? Math.max(0, DIGITS.indexOf(b[i])) : 0
    if (da === db) {
      out += DIGITS[da]
      i++
      continue
    }
    if (db - da > 1) return out + DIGITS[(da + db) >> 1]
    out += DIGITS[da]
    i++
    for (;;) {
      const d = i < av.length ? Math.max(0, DIGITS.indexOf(av[i])) : 0
      if (36 - d > 1) return out + DIGITS[(d + 36) >> 1]
      out += DIGITS[d]
      i++
    }
  }
}

/** Sibling order as the server sorts it: by sort_key, NULL last (a doc that
 * never got a key sits after every keyed one), ties keep input order. */
export function compareSortKey(a: string | null | undefined, b: string | null | undefined): number {
  if (a == null && b == null) return 0
  if (a == null) return 1
  if (b == null) return -1
  return a < b ? -1 : a > b ? 1 : 0
}

/** Key for a doc dropped at `index` of `siblings` (the dragged doc already
 * removed). Null-keyed siblings sort last on the server and bound nothing, so
 * the bounds are the nearest KEYED neighbours on each side — after every
 * keyed doc when only unkeyed ones follow. */
export function keyForPosition(siblings: { sort_key: string | null }[], index: number): string {
  let lo: string | null = null
  for (let i = Math.min(index, siblings.length) - 1; i >= 0; i--) {
    const k = siblings[i].sort_key
    if (k != null) {
      lo = k
      break
    }
  }
  let hi: string | null = null
  for (let i = Math.max(index, 0); i < siblings.length; i++) {
    const k = siblings[i].sort_key
    if (k != null) {
      hi = k
      break
    }
  }
  return keyBetween(lo, hi)
}

export function inferBlockType(content: string): string {
  if (content.startsWith('```mermaid')) return 'diagram_mermaid'
  if (content.startsWith('```d2')) return 'diagram_d2'
  if (content.startsWith('```') || content.startsWith('---')) return 'code'
  if (/^#{1,6} /.test(content)) return 'heading'
  if (content.startsWith('DECISION:')) return 'decision'
  return 'paragraph'
}

interface Placed {
  entry: Entry
  parent: string | null // block id, or the placeholder id for new headings
  tempId: string // entry id or a fresh uuid for inserts
}

/** Compute ops turning `baseline` into `entries`. */
export function computeOps(
  baseline: BaselineBlock[],
  entries: Entry[],
  newId: () => string = () => crypto.randomUUID(),
): Op[] {
  const old = new Map(baseline.map((b) => [b.id, b]))
  const ops: Op[] = []

  // 1. deletes: baseline ids that vanished
  const liveIds = new Set(entries.map((e) => e.id).filter(Boolean))
  for (const b of baseline) {
    if (!liveIds.has(b.id)) {
      ops.push({ kind: { op: 'delete', target: b.id }, source_refs: [] })
    }
  }

  // 2. compute parents via the importer's heading-stack rule
  const placed: Placed[] = []
  const stack: { level: number; id: string }[] = []
  for (const e of entries) {
    if (e.level > 0) {
      while (stack.length && stack[stack.length - 1].level >= e.level) stack.pop()
    }
    const parent = stack.length ? stack[stack.length - 1].id : null
    const tempId = e.id ?? newId()
    placed.push({ entry: e, parent, tempId })
    if (e.level > 0) stack.push({ level: e.level, id: tempId })
  }

  // 3. group by parent, keep a greedy increasing run of undisturbed survivors,
  //    key everything else between its neighbours
  const groups = new Map<string | null, Placed[]>()
  for (const p of placed) {
    if (!groups.has(p.parent)) groups.set(p.parent, [])
    groups.get(p.parent)!.push(p)
  }

  for (const group of groups.values()) {
    // pass 1: mark stable entries (id exists, parent unchanged, keys increasing)
    const stable = new Array<boolean>(group.length).fill(false)
    let lastKey = ''
    for (let i = 0; i < group.length; i++) {
      const { entry, parent } = group[i]
      if (!entry.id) continue
      const prev = old.get(entry.id)
      if (!prev || prev.parent !== parent) continue
      if (prev.order_key > lastKey) {
        stable[i] = true
        lastKey = prev.order_key
      }
    }
    // pass 2: assign keys + emit ops
    const keys = new Array<string | null>(group.length).fill(null)
    for (let i = 0; i < group.length; i++) {
      if (stable[i]) keys[i] = old.get(group[i].entry.id!)!.order_key
    }
    for (let i = 0; i < group.length; i++) {
      const { entry, parent, tempId } = group[i]
      const prevKey = i > 0 ? keys[i - 1] : null
      if (keys[i] == null) {
        let nextKey: string | null = null
        for (let j = i + 1; j < group.length; j++) {
          if (keys[j] != null) {
            nextKey = keys[j]
            break
          }
        }
        keys[i] = keyBetween(prevKey, nextKey)
      }
      const prev = entry.id ? old.get(entry.id) : undefined
      if (!prev) {
        ops.push({
          kind: {
            op: 'insert',
            block_id: tempId,
            parent_id: parent,
            order_key: keys[i]!,
            block_type: inferBlockType(entry.content),
            content: entry.content,
            refers_to: null,
          },
          source_refs: [],
        })
        continue
      }
      const moved = !stable[i] || prev.parent !== parent
      if (moved) {
        ops.push({
          kind: { op: 'move', target: entry.id, new_parent: parent, new_order_key: keys[i]! },
          source_refs: [],
        })
      }
      if (prev.content !== entry.content) {
        ops.push({
          kind: { op: 'replace', target: entry.id, content: entry.content },
          source_refs: [],
        })
      }
    }
  }

  return ops
}
