// The shared diff vectors: the Apple app's Swift port (TaisceKit DocDiff,
// DocDiffTests) reads the same file, so both editors turn the same baseline
// and entries into the same ops. Every vector, and random docs and edits,
// also check the invariant: the server's tree after the ops reads (pre-order,
// siblings by order key) in exactly the editor's order.
import { describe, expect, it } from 'vitest'
import { BaselineBlock, Entry, HiddenBlock, Op, computeOps, headingLevel, keyBetween } from './diff'
import vectorsJson from './diff.vectors.json'

interface Vector {
  name: string
  baseline: BaselineBlock[]
  entries: Entry[]
  /** blocks the editor doesn't show (frontmatter, comments): kept in place */
  hidden?: HiddenBlock[]
  ops: Record<string, unknown>[]
}

/** The editor's order with each hidden block after the visible block it
 * followed (or the nearest surviving one before that; else first). */
function withHidden(baseline: BaselineBlock[], ids: string[], hidden: HiddenBlock[]): string[] {
  const live = new Set(ids)
  const order = baseline.map((b) => b.id)
  const anchor = (a: string | null) => {
    for (let k = a == null ? -1 : order.indexOf(a); k >= 0; k--) if (live.has(order[k])) return order[k]
    return null
  }
  const out = hidden.filter((h) => anchor(h.after) == null).map((h) => h.id)
  for (const id of ids) {
    out.push(id)
    out.push(...hidden.filter((h) => anchor(h.after) === id).map((h) => h.id))
  }
  return out
}

const vectors = vectorsJson as unknown as { cases: Vector[] }

/** The server's blocks after `ops`, read in pre-order (siblings by key). */
export function serverOrder(baseline: BaselineBlock[], ops: Op[]): string[] {
  const blocks = new Map(baseline.map((b) => [b.id, { parent: b.parent, key: b.order_key }]))
  for (const { kind } of ops) {
    const k = kind as Record<string, unknown>
    if (k.op === 'delete') blocks.delete(k.target as string)
    if (k.op === 'insert') blocks.set(k.block_id as string, { parent: k.parent_id as string | null, key: k.order_key as string })
    if (k.op === 'move') {
      const b = blocks.get(k.target as string)!
      b.parent = k.new_parent as string | null
      b.key = k.new_order_key as string
    }
  }
  const kids = new Map<string | null, string[]>()
  for (const [id, b] of blocks) {
    const p = b.parent != null && blocks.has(b.parent) ? b.parent : null
    if (!kids.has(p)) kids.set(p, [])
    kids.get(p)!.push(id)
  }
  const out: string[] = []
  const walk = (p: string | null) => {
    const ids = (kids.get(p) ?? []).sort((a, b) => {
      const ka = blocks.get(a)!.key
      const kb = blocks.get(b)!.key
      return ka < kb ? -1 : ka > kb ? 1 : 0
    })
    for (const id of ids) {
      out.push(id)
      walk(id)
    }
  }
  walk(null)
  return out
}

function entryIds(entries: Entry[]): string[] {
  let n = 0
  return entries.map((e) => e.id ?? `new-${++n}`)
}

describe('shared diff vectors', () => {
  it('has cases', () => {
    expect(vectors.cases.length).toBeGreaterThan(10)
  })
  for (const c of vectors.cases) {
    it(c.name, () => {
      let n = 0
      const hidden = c.hidden ?? []
      const ops = computeOps(c.baseline, c.entries, () => `new-${++n}`, undefined, hidden)
      expect(ops.map((o) => o.kind)).toEqual(c.ops)
      expect(serverOrder([...c.baseline, ...hidden], ops)).toEqual(withHidden(c.baseline, entryIds(c.entries), hidden))
    })
  }
})

/** A small deterministic PRNG (mulberry32). */
function rng(seed: number) {
  return () => {
    seed |= 0
    seed = (seed + 0x6d2b79f5) | 0
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

describe('random docs and edits keep the server tree in the editor order', () => {
  it('holds for 2000 cases', () => {
    for (let seed = 1; seed <= 2000; seed++) {
      const r = rng(seed)
      const content = (i: number) => (r() < 0.35 ? `${'#'.repeat(1 + Math.floor(r() * 3))} H${i}` : `p${i}`)
      // a server tree whose pre-order is the doc order: each block's parent
      // is on the path to the block before it (or the top level)
      const baseline: BaselineBlock[] = []
      const path: string[] = []
      const lastKey = new Map<string | null, string | null>()
      const count = 1 + Math.floor(r() * 9)
      for (let i = 0; i < count; i++) {
        const id = `b${i}`
        const depth = Math.floor(r() * (path.length + 1))
        path.length = depth
        const parent = depth > 0 ? path[depth - 1] : null
        const key = keyBetween(lastKey.get(parent) ?? null, null)
        lastKey.set(parent, key)
        baseline.push({ id, content: content(i), parent, order_key: key })
        path.push(id)
      }
      // entries: the baseline, edited at random
      let entries: Entry[] = baseline.map((b) => ({ id: b.id, content: b.content, level: headingLevel(b.content) }))
      const edits = Math.floor(r() * 4)
      for (let k = 0; k < edits; k++) {
        const what = r()
        const at = Math.floor(r() * (entries.length + 1))
        if (what < 0.25 && entries.length > 0) {
          entries.splice(Math.min(at, entries.length - 1), 1)
        } else if (what < 0.5) {
          const c = content(100 + k)
          entries.splice(at, 0, { id: null, content: c, level: headingLevel(c) })
        } else if (what < 0.75 && entries.length > 0) {
          const i = Math.min(at, entries.length - 1)
          const c = r() < 0.5 ? `p${i}x` : `${'#'.repeat(1 + Math.floor(r() * 3))} T${i}`
          entries[i] = { ...entries[i], content: c, level: headingLevel(c) }
        } else if (entries.length > 1) {
          const from = Math.floor(r() * entries.length)
          const [e] = entries.splice(from, 1)
          entries.splice(Math.floor(r() * (entries.length + 1)), 0, e)
        }
      }
      entries = entries.filter((e, i, xs) => e.id == null || xs.findIndex((x) => x.id === e.id) === i)
      let n = 0
      const ops = computeOps(baseline, entries, () => `new-${++n}`)
      const got = serverOrder(baseline, ops)
      expect(got, `seed ${seed}`).toEqual(entryIds(entries))
    }
  })
})

/** Heading-rule parents for a sequence of (id, level). */
function inferredParents(items: { id: string; level: number }[]): Map<string, string | null> {
  const out = new Map<string, string | null>()
  const st: { level: number; id: string }[] = []
  for (const it of items) {
    if (it.level > 0) while (st.length && st[st.length - 1].level >= it.level) st.pop()
    out.set(it.id, st.length ? st[st.length - 1].id : null)
    if (it.level > 0) st.push({ level: it.level, id: it.id })
  }
  return out
}

describe('saves move only what the edit re-homed', () => {
  const modes = ['heading', 'flat', 'random'] as const
  // one test per nesting style (each a fraction of a second; one test for
  // all three ran past vitest's 5 s timeout on a loaded machine)
  for (const mode of modes) {
    it(`${mode} docs: typos and retitles are one replace; a level change moves only changed sections (or out-of-order blocks)`, () => {
      const counts = { cases: 0, bursts: 0, extra: 0 }
      for (let seed = 1; seed <= 1000; seed++) {
        const r = rng(seed * 7 + mode.length)
        const n = 4 + Math.floor(r() * 10)
        const contents = Array.from({ length: n }, (_, i) => (r() < 0.4 ? `${'#'.repeat(1 + Math.floor(r() * 3))} H${i}` : `p${i}`))
        const items = contents.map((c, i) => ({ id: `b${i}`, level: headingLevel(c) }))
        const inferredBase = inferredParents(items)
        const baseline: BaselineBlock[] = []
        const lastKey = new Map<string | null, string | null>()
        const path: string[] = []
        contents.forEach((c, i) => {
          const id = `b${i}`
          let parent: string | null
          if (mode === 'heading') parent = inferredBase.get(id) ?? null
          else if (mode === 'flat') parent = null
          else {
            const depth = Math.floor(r() * (path.length + 1))
            path.length = depth
            parent = depth > 0 ? path[depth - 1] : null
            path.push(id)
          }
          const key = keyBetween(lastKey.get(parent) ?? null, null)
          lastKey.set(parent, key)
          baseline.push({ id, content: c, parent, order_key: key })
        })
        const entries = (): Entry[] => baseline.map((b) => ({ id: b.id, content: b.content, level: headingLevel(b.content) }))
        // a paragraph typo
        const paras = baseline.filter((b) => headingLevel(b.content) === 0)
        if (paras.length) {
          const e = entries()
          const pick = paras[Math.floor(r() * paras.length)].id
          const t = e.find((x) => x.id === pick)!
          t.content += ' typo'
          const ops = computeOps(baseline, e, () => 'new')
          expect(ops.map((o) => o.kind.op), `${mode} ${seed} typo`).toEqual(['replace'])
        }
        const heads = baseline.filter((b) => headingLevel(b.content) > 0)
        if (!heads.length) continue
        // a heading retitled at its level
        {
          const e = entries()
          const pickH = heads[Math.floor(r() * heads.length)].id
          const t = e.find((x) => x.id === pickH)!
          t.content += ' retitled'
          const ops = computeOps(baseline, e, () => 'new')
          expect(ops.map((o) => o.kind.op), `${mode} ${seed} retitle`).toEqual(['replace'])
        }
        // a heading's level changed
        const e = entries()
        const pickL = heads[Math.floor(r() * heads.length)].id
        const t = e.find((x) => x.id === pickL)!
        const lv = 1 + ((t.level + Math.floor(r() * 2)) % 3)
        t.level = lv === t.level ? (lv % 3) + 1 : lv
        t.content = `${'#'.repeat(t.level)} ${t.content.replace(/^#+ /, '')}`
        const stats = { orderFallback: [] as string[] }
        const ops = computeOps(baseline, e, () => 'new', stats)
        expect(serverOrder(baseline, ops), `${mode} ${seed} order`).toEqual(e.map((x) => x.id!))
        const after = inferredParents(e.map((x) => ({ id: x.id!, level: x.level })))
        const changed = new Set(e.filter((x) => after.get(x.id!) !== inferredBase.get(x.id!)).map((x) => x.id!))
        changed.add(t.id!)
        const moved = ops.filter((o) => o.kind.op === 'move').map((o) => o.kind.target as string)
        const allowed = new Set([...changed, ...stats.orderFallback])
        expect(moved.filter((m) => !allowed.has(m)), `${mode} ${seed} level change moved untouched blocks`).toEqual([])
        counts.cases++
        if (moved.length > n / 2) counts.bursts++
        counts.extra += moved.filter((m) => !changed.has(m)).length
      }
      // bursts = a level change moving more than half the doc; extra = moves
      // of blocks whose section didn't change (all from the order check).
      // Measured: heading 0/0, flat 0/0, random 2 bursts / 40 extra.
      if (mode === 'random') expect(counts.bursts).toBeLessThanOrEqual(5)
      else expect(counts).toMatchObject({ bursts: 0, extra: 0 })
    })
  }
})
