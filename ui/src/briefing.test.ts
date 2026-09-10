import { describe, expect, it } from 'vitest'
import {
  carryForward,
  dayBefore,
  extractPlans,
  extractSectionLines,
  findDailyDoc,
  groupQueueByDoc,
  localDateTitle,
  runVerdictCounts,
  sinceItems,
  toggleCheckboxLine,
  verdictLine,
} from './briefing'
import type { ActivityItem, Doc, GardenerRun, QueueRow } from './types'

function row(doc: string, title: string, proposer: string, id: string): QueueRow {
  return {
    item: {
      annotation: { id, doc_id: doc, op_id: `op-${id}`, kind: 'parked', status: 'open' },
      op: {
        id: `op-${id}`,
        kind: { op: 'replace', target: 'b', content: 'x' },
        principal: 'p',
        base_epoch: 1,
        epoch_applied: null,
        verdict: 'red',
        confidence: null,
        prior: null,
        source_refs: [],
      },
    },
    doc_title: title,
    proposer,
    current_content: null,
  }
}

describe('groupQueueByDoc', () => {
  it('groups by doc in first-seen order with unique proposers', () => {
    const g = groupQueueByDoc([
      row('d1', 'Roadmap', 'scribe', 'a1'),
      row('d2', 'Notes', 'alice', 'a2'),
      row('d1', 'Roadmap', 'tagger', 'a3'),
      row('d1', 'Roadmap', 'scribe', 'a4'),
    ])
    expect(g.map((x) => x.docId)).toEqual(['d1', 'd2'])
    expect(g[0].annotationIds).toEqual(['a1', 'a3', 'a4'])
    expect(g[0].proposers).toEqual(['scribe', 'tagger'])
    expect(g[1].title).toBe('Notes')
  })
})

describe('runVerdictCounts', () => {
  it('reads counts out of a run summary', () => {
    expect(runVerdictCounts('verdicts: 3 yellow, 1 red')).toEqual({ green: 0, yellow: 3, red: 1 })
    expect(verdictLine(runVerdictCounts('2 green and 1 yellow'))).toBe('2 green · 1 yellow')
    expect(runVerdictCounts('nothing to do')).toBeNull()
    expect(runVerdictCounts(null)).toBeNull()
  })
})

describe('sinceItems', () => {
  const now = new Date('2026-09-10T12:00:00Z').getTime()
  const runs: GardenerRun[] = [
    { id: 'r1', gardener: 'g', gardener_name: 'tagger', started_at: '2026-09-10T09:00:00Z', status: 'ok', summary: '2 yellow', tokens_used: null, tool_calls: null },
    { id: 'r0', gardener: 'g', gardener_name: 'tagger', started_at: '2026-09-08T09:00:00Z', status: 'ok', summary: null, tokens_used: null, tool_calls: null },
  ]
  const activity: ActivityItem[] = [
    { op_id: 'o1', doc_id: 'd', doc_title: 'Notes', principal: 'p', principal_name: 'alice', op_type: 'replace', epoch: 3, created_at: '2026-09-10T10:00:00Z' },
  ]
  const docs = [{ id: 'n', title: 'Answer', created_by_name: 'scribe', created_by_kind: 'agent', created_at: '2026-09-10T11:00:00Z' }]

  it('merges newest-first and drops anything before the stamp', () => {
    const items = sinceItems('2026-09-09T00:00:00Z', runs, activity, docs, now)
    expect(items.map((i) => i.kind)).toEqual(['newdoc', 'edit', 'run'])
    expect(items[2].detail).toBe('ok · 2 yellow')
    expect(items[1].text).toBe('alice edited “Notes”')
  })
  it('without a stamp shows the last 24h', () => {
    expect(sinceItems(null, runs, [], [], now)).toHaveLength(1)
    expect(sinceItems('2026-09-10T11:30:00Z', runs, activity, docs, now)).toHaveLength(0)
  })
})

describe('daily doc lookup', () => {
  it('formats local dates and steps back a day across month starts', () => {
    expect(localDateTitle(new Date(2026, 8, 10))).toBe('2026-09-10')
    expect(dayBefore('2026-09-01')).toBe('2026-08-31')
    expect(dayBefore('2026-01-01')).toBe('2025-12-31')
  })
  it('finds the titled doc only under the Daily root', () => {
    const doc = (id: string, title: string, parent_id: string | null): Doc => ({
      id,
      title,
      parent_id,
      review_policy: null,
      current_epoch: 0,
      created_by: 'p',
      status: null,
      sort_key: null,
    })
    const docs = [
      doc('daily', 'Daily', null),
      doc('sep', '2026-09', 'daily'),
      doc('t', '2026-09-10', 'sep'),
      doc('elsewhere', '2026-09-11', null),
    ]
    expect(findDailyDoc(docs, '2026-09-10')?.id).toBe('t')
    expect(findDailyDoc(docs, '2026-09-11')).toBeNull()
    expect(findDailyDoc([doc('x', '2026-09-10', null)], '2026-09-10')).toBeNull()
  })
})

describe('plans', () => {
  const blocks = [
    { id: 'h1', content: '## grimoire' },
    { id: 'h2', content: '### Done' },
    { id: 'b1', content: '- shipped the thing' },
    { id: 'h3', content: '### Plans' },
    { id: 'b2', content: '- [ ] write the omnibox\n- [x] home view\n  - [ ] nested item' },
    { id: 'h4', content: '### Open Questions' },
    { id: 'b3', content: '- keep ⌘O as an alias?\n- what about ⌘S' },
    { id: 'h5', content: '## qompass' },
    { id: 'h6', content: '### Plans' },
    { id: 'b4', content: '- [ ] rotate the token' },
    { id: 'h7', content: '## other' },
    { id: 'b5', content: '- [ ] not under Plans' },
  ]
  it('extracts checkboxes under ### Plans grouped by ## project', () => {
    const g = extractPlans(blocks)
    expect(g.map((x) => x.project)).toEqual(['grimoire', 'qompass'])
    expect(g[0].items).toEqual([
      { blockId: 'b2', line: 0, text: 'write the omnibox', checked: false },
      { blockId: 'b2', line: 1, text: 'home view', checked: true },
      { blockId: 'b2', line: 2, text: 'nested item', checked: false },
    ])
    expect(g[1].items[0].text).toBe('rotate the token')
  })
  it('extracts open questions per project', () => {
    expect(extractSectionLines(blocks, 'Open Questions')).toEqual([
      { project: 'grimoire', lines: ['keep ⌘O as an alias?', 'what about ⌘S'] },
    ])
  })
  it('toggles exactly one line and leaves non-checkbox lines alone', () => {
    const c = '- [ ] a\n- [x] b\nplain'
    expect(toggleCheckboxLine(c, 0)).toBe('- [x] a\n- [x] b\nplain')
    expect(toggleCheckboxLine(c, 1)).toBe('- [ ] a\n- [ ] b\nplain')
    expect(toggleCheckboxLine(c, 2)).toBe(c)
    expect(toggleCheckboxLine(c, 9)).toBe(c)
    expect(toggleCheckboxLine('  - [ ] nested', 0)).toBe('  - [x] nested')
  })
})

describe('carryForward', () => {
  it('appends to an existing section, attached to the list', () => {
    const md = '## grimoire\n\n### Plans\n\n- [ ] already here\n\n### Notable\n\n- x\n'
    const out = carryForward(md, 'grimoire', 'Plans', ['omnibox', 'already here'], '2026-09-09')
    expect(out).toBe(
      '## grimoire\n\n### Plans\n\n- [ ] already here\n- [ ] omnibox (carried from 2026-09-09)\n\n### Notable\n\n- x\n',
    )
  })
  it('creates the ### section inside an existing project', () => {
    const md = '## grimoire\n\n### Done\n\n- shipped\n\n## qompass\n\n### Plans\n\n- [ ] q\n'
    const out = carryForward(md, 'grimoire', 'Open Questions', ['why?'], '2026-09-09')
    expect(out).toBe(
      '## grimoire\n\n### Done\n\n- shipped\n\n### Open Questions\n- why? (carried from 2026-09-09)\n\n## qompass\n\n### Plans\n\n- [ ] q\n',
    )
  })
  it('creates the project and section when the doc is empty or lacks them', () => {
    expect(carryForward('', 'grimoire', 'Plans', ['a'], '2026-09-09')).toBe(
      '## grimoire\n\n### Plans\n- [ ] a (carried from 2026-09-09)\n',
    )
    const out = carryForward('## qompass\n\n### Plans\n\n- [ ] q\n', 'grimoire', 'Plans', ['a'], '2026-09-09')
    expect(out).toBe('## qompass\n\n### Plans\n\n- [ ] q\n\n## grimoire\n\n### Plans\n- [ ] a (carried from 2026-09-09)\n')
  })
  it('is a no-op when every line is already present (stamp ignored)', () => {
    const md = '## g\n\n### Plans\n\n- [x] a (carried from 2026-09-08)\n'
    expect(carryForward(md, 'g', 'Plans', ['a'], '2026-09-09')).toBe(md)
  })
})
