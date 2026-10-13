// The shared diff vectors: the Apple app's Swift port (TaisceKit DocDiff,
// DocDiffTests) reads the same file, so both editors turn the same baseline
// and entries into the same ops.
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { BaselineBlock, Entry, computeOps } from './diff'

interface Vector {
  name: string
  baseline: BaselineBlock[]
  entries: Entry[]
  ops: Record<string, unknown>[]
}

const vectors: { cases: Vector[] } = JSON.parse(
  readFileSync(new URL('./diff.vectors.json', import.meta.url), 'utf8'),
)

describe('shared diff vectors', () => {
  it('has cases', () => {
    expect(vectors.cases.length).toBeGreaterThan(10)
  })
  for (const c of vectors.cases) {
    it(c.name, () => {
      let n = 0
      const ops = computeOps(c.baseline, c.entries, () => `new-${++n}`)
      expect(ops.map((o) => o.kind)).toEqual(c.ops)
    })
  }
})
