// The shared diff vectors: the Apple app's Swift port (TaisceKit DocDiff,
// DocDiffTests) reads the same file, so both editors turn the same baseline
// and entries into the same ops. Every vector, and random docs and edits,
// also check the invariant: the server's tree after the ops reads (pre-order,
// siblings by order key) in exactly the editor's order.
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { BaselineBlock, Entry, Op, computeOps, headingLevel, keyBetween } from './diff'

interface Vector {
  name: string
  baseline: BaselineBlock[]
  entries: Entry[]
  ops: Record<string, unknown>[]
}

const vectors: { cases: Vector[] } = JSON.parse(
  readFileSync(new URL('./diff.vectors.json', import.meta.url), 'utf8'),
)

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
      const ops = computeOps(c.baseline, c.entries, () => `new-${++n}`)
      expect(ops.map((o) => o.kind)).toEqual(c.ops)
      expect(serverOrder(c.baseline, ops)).toEqual(entryIds(c.entries))
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
