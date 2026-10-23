import { describe, expect, it } from 'vitest'
import { backlinkSnippet, contributors, frontmatterTags, versions, withTag, wordDiff } from './docMeta'
import type { HistoryRow } from '../types'

describe('frontmatterTags', () => {
  it('reads inline, comma and list forms, deduped, without #', () => {
    expect(frontmatterTags(['---\ntags: [a, "b"]\n---'])).toEqual(['a', 'b'])
    expect(frontmatterTags(['---\ntitle: x\ntags: c, #d\n---'])).toEqual(['c', 'd'])
    expect(frontmatterTags(['---\ntags:\n  - e\n  - f\nother: 1\n---', '---\ntags: [e]\n---'])).toEqual(['e', 'f'])
    expect(frontmatterTags(['not frontmatter\ntags: x'])).toEqual([])
  })
})

describe('withTag', () => {
  it('adds to each form, or makes a frontmatter block', () => {
    expect(withTag(null, 'x')).toBe('---\ntags: [x]\n---')
    expect(withTag('---\ntags: [a]\n---', 'x')).toBe('---\ntags: [a, x]\n---')
    expect(withTag('---\ntags: []\n---', 'x')).toBe('---\ntags: [x]\n---')
    expect(withTag('---\ntags: a, b\n---', 'x')).toBe('---\ntags: a, b, x\n---')
    expect(withTag('---\ntags:\n  - a\n---', 'x')).toBe('---\ntags:\n  - a\n  - x\n---')
    expect(withTag('---\ntitle: t\n---', 'x')).toBe('---\ntags: [x]\ntitle: t\n---')
    expect(frontmatterTags([withTag('---\ntags:\n  - a\n---', 'x')])).toEqual(['a', 'x'])
  })
})

describe('wordDiff', () => {
  const join = (t: 'eq' | 'del' | 'ins', segs: ReturnType<typeof wordDiff>) =>
    segs.filter((s) => s.t === 'eq' || s.t === t).map((s) => s.s).join('')
  it('rebuilds both sides exactly', () => {
    const a = "Every change is a proposal. Agents' edits are always yellow."
    const b = 'Every change is a proposal. An edit against a block lands yellow; a delete is always red.'
    const d = wordDiff(a, b)
    expect(join('del', d)).toBe(a)
    expect(join('ins', d)).toBe(b)
  })
  it('reads as one struck run and one new run, not a word salad', () => {
    const d = wordDiff("Agents' edits are always yellow.", 'An edit against a block that changed lands yellow; a delete is always parked red.')
    expect(d.filter((s) => s.t !== 'eq').length).toBeLessThanOrEqual(2)
  })
  it('keeps a long unchanged run', () => {
    const d = wordDiff('one two three four five six', 'one two three four five seven')
    expect(d[0]).toEqual({ t: 'eq', s: 'one two three four five ' })
  })
  it('handles empty sides', () => {
    expect(wordDiff('', 'new')).toEqual([{ t: 'ins', s: 'new' }])
    expect(wordDiff('old', '')).toEqual([{ t: 'del', s: 'old' }])
  })
})

const row = (epoch: number | null, name: string, kind: string, op: string, yours = false, verdict: 'green' | 'yellow' | 'red' = 'green') =>
  ({
    op: { id: `${epoch}-${op}-${Math.random()}`, kind: { op }, principal: name, base_epoch: 0, epoch_applied: epoch, verdict, confidence: null, prior: null, source_refs: [] },
    principal_name: name,
    principal_kind: kind,
    principal_is_yours: yours,
  }) as HistoryRow & { principal_is_yours: boolean }

describe('contributors and versions', () => {
  const rows = [row(3, 'keeper:API', 'agent', 'replace', false, 'yellow'), row(2, 'Tom', 'human', 'replace', true), row(2, 'Tom', 'human', 'insert', true), row(1, 'claude:docs', 'agent', 'insert')]
  it('puts you first, then the most recent', () => {
    expect(contributors(rows).map((c) => [c.name, c.agent, c.yours])).toEqual([
      ['Tom', false, true],
      ['keeper:API', true, false],
      ['claude:docs', true, false],
    ])
  })
  it('groups by version, newest first, with a summary and the worst verdict', () => {
    const v = versions(rows)
    expect(v.map((x) => x.epoch)).toEqual([3, 2, 1])
    expect(v[0]).toMatchObject({ who: 'keeper:API', agent: true, verdict: 'yellow', summary: 'edited 1 block' })
    expect(v[1].summary).toBe('edited 1 block, inserted 1 block')
    expect(versions([row(null, 'x:y', 'agent', 'replace', false, 'red')])[0].summary).toBe('proposed: edited 1 block')
  })
})

describe('backlinkSnippet', () => {
  it('centres on the link', () => {
    const s = backlinkSnippet('A long preamble that goes on and on and on before the point where it says [[Architecture]] and then more text follows after it for a while.', 'Architecture', 20)
    expect(s.startsWith('…')).toBe(true)
    expect(s).toContain('[[Architecture]]')
    expect(s.endsWith('…')).toBe(true)
  })
})

import { plainLine } from '../screens/DocInspector'
describe('plainLine', () => {
  it('drops markdown marks', () => {
    expect(plainLine('- **Store** (`taisce-store`): see [[Review Gate|the gate]] and [[Roadmap]]\nmore')).toBe(
      'Store (taisce-store): see the gate and Roadmap',
    )
    expect(plainLine('## Layers')).toBe('Layers')
  })
})
