import { describe, expect, it } from 'vitest'
import type { BlockNode } from '../types'
import { answerRuns, answerTitle, flatText, highlight, numberCitations, parseAnswer, queryTerms, sameQuestion, shortWhen, snippetAround, uuidTime } from './searchText'
import { excerptOf, headingOf } from './docCache'

const ID1 = '01a169c4-da7a-7af0-b03a-657389be43f7'
const ID2 = '01a169c4-da7a-7af0-b03a-6581e3a877df'
const ID3 = '01a169c4-da75-7581-b460-91c9fee3812e'

let n = 0
function node(content: string, children: BlockNode[] = [], block_type = 'paragraph'): BlockNode {
  n += 1
  return {
    block: { id: `b${n}`, doc_id: 'd', parent_id: null, order_key: 'i', block_type, content, created_by: 'p', epoch: 1, deleted: false, refers_to: null },
    children,
  }
}

describe('highlight', () => {
  it('marks query words case-insensitively, skipping little words', () => {
    const segs = highlight('The Review gate scores it', 'how does the review gate')
    expect(segs.filter((s) => s.m).map((s) => s.t)).toEqual(['Review', 'gate'])
    expect(segs.map((s) => s.t).join('')).toBe('The Review gate scores it')
  })
  it('longest terms first, regex characters are literal', () => {
    expect(queryTerms('re review (x)')).toEqual(['review', 're'])
    expect(highlight('a (b) c', '(b)').some((s) => s.m)).toBe(false) // "b" is 1 char
    expect(highlight('cost $12 (net)', '$12 (net').filter((s) => s.m).map((s) => s.t)).toEqual(['12', 'net'])
  })
  it('nothing to mark → one plain run', () => {
    expect(highlight('plain text', '')).toEqual([{ t: 'plain text' }])
  })
})

describe('snippets', () => {
  it('flattens markdown into one line', () => {
    expect(flatText('## Verdicts\n- **green** is [[Review Gate#^x|the gate]]\n```rust\nfn x()\n```')).toBe('Verdicts · green is the gate · fn x()')
  })
  it('centres a long block on the first match', () => {
    const long = `${'lorem ipsum '.repeat(40)}the needle sits here ${'dolor sit '.repeat(30)}`
    const s = snippetAround(long, 'needle', 120)
    expect(s.startsWith('…')).toBe(true)
    expect(s).toContain('needle')
    expect(s.length).toBeLessThanOrEqual(124)
  })
  it('a short block is shown whole', () => {
    expect(snippetAround('short one', 'one')).toBe('short one')
  })
})

describe('time', () => {
  it('reads the ms out of a UUIDv7, null for others', () => {
    expect(uuidTime('01a169c4-da86-7143-943e-d263ded138bc')).toBe(0x01a169c4da86)
    expect(uuidTime('123e4567-e89b-42d3-a456-426614174000')).toBeNull()
  })
  it('short Updated labels', () => {
    const now = new Date('2026-10-23T15:00:00').getTime()
    expect(shortWhen(now - 12 * 60_000, now)).toBe('12 min')
    expect(shortWhen(now - 20_000, now)).toBe('now')
  })
})

describe('answers', () => {
  const doc = [
    node('---\nquestion: "How does the gate pick?"\n---', [], 'code'),
    node('# How does the gate pick', [
      node(`Deletes are **parked** [[Architecture#^${ID3}]]. Stale edits land yellow [[Agent Guide#^${ID2}]] [[Architecture#^${ID3}]].\n\nEverything else is green.`),
      node('## Agent Guide', [
        node(`> How an agent should write here\n>\n> — [[Agent Guide#^${ID1}]]`),
        node(`> 1. \`find_doc\` the target\n>\n> — [[Agent Guide#^${ID2}]]`),
      ]),
      node('## Architecture', [node(`> \`\`\`rust pub enum Verdict \`\`\`\n>\n> — [[Architecture#^${ID3}]]`), node('---'), node('*Asked 2026-10-23 · 3 blocks across 2 docs.*')]),
    ]),
  ]
  it('reads the question, prose, receipts and meta back', () => {
    const a = parseAnswer(doc)
    expect(a.question).toBe('How does the gate pick')
    expect(a.paras).toHaveLength(2)
    expect(a.writing).toBe(false)
    expect(a.receipts.map((r) => [r.title, r.blockId])).toEqual([
      ['Agent Guide', ID1],
      ['Agent Guide', ID2],
      ['Architecture', ID3],
    ])
    expect(a.receipts[2].quote).toBe('pub enum Verdict')
    expect(a.meta).toBe('Asked 2026-10-23 · 3 blocks across 2 docs.')
  })
  it('numbers citations by first appearance; uncited receipts trail', () => {
    const { num, cited, uncited } = numberCitations(parseAnswer(doc))
    expect([...num.entries()]).toEqual([
      [ID3, 1],
      [ID2, 2],
    ])
    expect(cited.map((r) => r.blockId)).toEqual([ID3, ID2])
    expect(uncited.map((r) => r.blockId)).toEqual([ID1])
  })
  it('the placeholder means still writing; no synthesis means receipts only', () => {
    const writing = parseAnswer([node('# Q', [node('*✨ Writing a synthesis from the excerpts below…*'), node(`> x\n>\n> — [[D#^${ID1}]]`)])])
    expect(writing.writing).toBe(true)
    expect(writing.paras).toHaveLength(0)
    const bare = parseAnswer([node('# Q', [node('## D', [node(`> x\n>\n> — [[D#^${ID1}]]`)])])])
    expect(bare.paras).toHaveLength(0)
    expect(bare.receipts).toHaveLength(1)
  })
  it('prose runs: bold, italic, citations, plain wikilinks as titles', () => {
    expect(answerRuns(`A **b** *c* [[Doc]] d [[T#^${ID1}]]`)).toEqual([
      { t: 'A ' },
      { t: 'b', b: true },
      { t: ' ' },
      { t: 'c', i: true },
      { t: ' Doc d ' },
      { cite: ID1, title: 'T' },
    ])
  })
  it('answer titles follow ask.rs title_for', () => {
    expect(answerTitle('  What is it??  ')).toBe('What is it')
    expect(answerTitle('x'.repeat(90))).toHaveLength(81)
    expect(sameQuestion('How does it work?', 'how does it work')).toBe(true)
  })
})

describe('doc helpers', () => {
  const tree = [node('---\ntags: [a]\n---', [], 'code'), node('## Verdicts', [node('| Green | Applied |')], 'heading'), node('Plain first line\nsecond')]
  it('headingOf finds the section a block sits in', () => {
    const inner = tree[1].children[0].block.id
    expect(headingOf(tree, inner)).toBe('Verdicts')
    expect(headingOf(tree, tree[2].block.id)).toBeNull()
  })
  it('excerptOf skips frontmatter and headings', () => {
    expect(excerptOf(tree)).toBe('| Green | Applied |')
    expect(excerptOf([node('# T'), node('- [ ] buy **milk**')])).toBe('buy milk')
  })
})
