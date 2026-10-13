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

/** A block the editor doesn't show, and the visible block it follows in
 * the doc (null: before every visible one). */
export interface HiddenBlock extends BaselineBlock {
  after: string | null
}

/** The baseline and entries with the hidden blocks in their places: each
 * after the visible block it followed, or the nearest surviving one before
 * that (first, if none). They are unchanged entries, so they are never
 * written, only keyed around. */
export function withHiddenBlocks(baseline: BaselineBlock[], entries: Entry[], hidden: HiddenBlock[]): [BaselineBlock[], Entry[]] {
  const order = baseline.map((b) => b.id)
  const live = new Set(entries.map((e) => e.id).filter((x): x is string => x != null))
  const anchor = (a: string | null): string | null => {
    for (let k = a == null ? -1 : order.indexOf(a); k >= 0; k--) if (live.has(order[k])) return order[k]
    return null
  }
  const asEntry = (h: HiddenBlock): Entry => ({ id: h.id, content: h.content, level: 0 })
  const plain = (h: HiddenBlock): BaselineBlock => ({ id: h.id, content: h.content, parent: h.parent, order_key: h.order_key })
  const outEntries: Entry[] = hidden.filter((h) => anchor(h.after) == null).map(asEntry)
  for (const e of entries) {
    outEntries.push(e)
    if (e.id != null) outEntries.push(...hidden.filter((h) => anchor(h.after) === e.id).map(asEntry))
  }
  // the baseline in doc order too (the sections before the edit)
  const known = new Set(order)
  const outBase: BaselineBlock[] = hidden.filter((h) => h.after == null || !known.has(h.after)).map(plain)
  for (const b of baseline) {
    outBase.push(b)
    outBase.push(...hidden.filter((h) => h.after === b.id).map(plain))
  }
  return [outBase, outEntries]
}

/** A block's heading level from its markdown (0 = not a heading). */
export function headingLevel(content: string): number {
  const m = /^(#{1,6}) /.exec(content)
  return m ? m[1].length : 0
}

/** Pre-order of the tree the placements make (children in entries order;
 * a parent that isn't an entry counts as the top level). Returns the
 * indices [came early, expected] at the first place it differs from the
 * entries order, or null when it reads in order. */
export function firstOutOfOrder(placed: { tempId: string; parent: string | null }[]): [number, number] | null {
  const index = new Map(placed.map((p, i) => [p.tempId, i]))
  const kids = new Map<number | null, number[]>()
  placed.forEach((p, i) => {
    const k = p.parent != null && index.has(p.parent) ? index.get(p.parent)! : null
    if (!kids.has(k)) kids.set(k, [])
    kids.get(k)!.push(i)
  })
  const order: number[] = []
  const seen = new Set<number>()
  const walk = (k: number | null) => {
    for (const c of kids.get(k) ?? []) {
      if (seen.has(c)) continue // a cycle never reads in order
      seen.add(c)
      order.push(c)
      walk(c)
    }
  }
  walk(null)
  for (let i = 0; i < placed.length; i++) {
    if (order[i] !== i) {
      // unreachable (a cycle): the expected one is out of place
      return [order[i] ?? i, i]
    }
  }
  return null
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
  /** filled in for tests: blocks the order check (2c) re-homed */
  stats?: { orderFallback: string[] },
  /** blocks the editor doesn't show (frontmatter, comments): they keep
   * their place next to the visible block they followed, so a new block
   * never lands before a doc's frontmatter or takes its key */
  hidden: HiddenBlock[] = [],
): Op[] {
  if (hidden.length) [baseline, entries] = withHiddenBlocks(baseline, entries, hidden)
  const hiddenIds = new Set(hidden.map((h) => h.id))
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

  // 2b. where each baseline block belonged before this edit, by the same
  //     rule. A block keeps its STORED parent unless this edit changed
  //     where it belongs (or deleted that parent): nesting the server holds
  //     that disagrees with the headings is left alone, so a typo fix never
  //     re-parents anything.
  const before = new Map<string, string | null>()
  {
    const st: { level: number; id: string }[] = []
    for (const b of baseline) {
      const lv = headingLevel(b.content)
      if (lv > 0) {
        while (st.length && st[st.length - 1].level >= lv) st.pop()
      }
      before.set(b.id, st.length ? st[st.length - 1].id : null)
      if (lv > 0) st.push({ level: lv, id: b.id })
    }
  }
  //     A new or re-homed block goes where its neighbour above went when
  //     both belong to the same section, so it lands next to it.
  const inferred = placed.map((p) => p.parent)
  placed.forEach((p, i) => {
    const id = p.entry.id
    const prev = id ? old.get(id) : undefined
    // kept: its section didn't change, or the server never nested it by
    // its section anyway (flat or misnested): a heading edit elsewhere
    // says nothing about where it belongs
    if (prev && (before.get(prev.id) === inferred[i] || prev.parent !== before.get(prev.id))) {
      const storedGone = prev.parent != null && old.has(prev.parent) && !liveIds.has(prev.parent)
      if (!storedGone) {
        p.parent = prev.parent
        return
      }
    }
    // (the neighbour is the nearest visible one: a hidden comment's parent
    // says nothing about where new text goes)
    let j = i - 1
    while (j >= 0 && placed[j].entry.id != null && hiddenIds.has(placed[j].entry.id!)) j--
    if (j >= 0 && inferred[j] === inferred[i]) p.parent = placed[j].parent
  })

  // 2c. the server's tree must read in the editor's order: walk it in
  //     pre-order (siblings in entries order, as step 3 keys them). At the
  //     first block out of place, the one that came too early goes back to
  //     its inferred parent (or, if it is there already, the one expected).
  //     All-inferred nesting reads in order by construction: the last resort.
  const tried = placed.map(() => 0)
  for (let guard = 0; ; guard++) {
    const bad = firstOutOfOrder(placed)
    if (bad == null) break
    if (guard > 2 * placed.length) {
      placed.forEach((p, i) => (p.parent = inferred[i]))
      stats?.orderFallback.push(...placed.map((p) => p.tempId))
      break
    }
    // each block steps at most twice: back to where the server has it
    // (moving nothing), then to its inferred parent
    const [early, expected] = bad
    const step = (i: number): boolean => {
      const stored = placed[i].entry.id ? old.get(placed[i].entry.id!)?.parent : undefined
      const storedLive = stored !== undefined && (stored == null || !old.has(stored) || liveIds.has(stored))
      if (tried[i] === 0 && storedLive && placed[i].parent !== stored) {
        placed[i].parent = stored!
        tried[i] = 1
        return true
      }
      if (tried[i] < 2 && placed[i].parent !== inferred[i]) {
        placed[i].parent = inferred[i]
        tried[i] = 2
        stats?.orderFallback.push(placed[i].tempId)
        return true
      }
      tried[i] = 2
      return false
    }
    if (!step(early) && !step(expected)) guard = 2 * placed.length
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
