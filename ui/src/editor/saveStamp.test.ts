// After a save, every new block's id must land on the node it came from.
// computeOps emits inserts grouped by parent, not in doc order: pairing
// id-less nodes with inserts by position gave blocks each other's ids, and
// the next edit then overwrote the wrong block on the server.
import { describe, expect, it } from 'vitest'
import { EditorState } from '@tiptap/pm/state'
import type { Node as PMNode } from '@tiptap/pm/model'
import type { Block } from '../types'
import type { BaselineBlock, Op } from './diff'
import { applyStamps, entriesOf, loadDoc, parser, planSave, rebuildBaseline, schema } from './DocEditor'

function block(id: string, content: string, parent_id: string | null, order_key: string): Block {
  return { id, doc_id: 'd', parent_id, order_key, block_type: 'paragraph', content, created_by: 'me', epoch: 1, deleted: false, refers_to: null }
}

/** the loaded doc's nodes, then `typed` (markdown, no ids) appended */
function docWith(blocks: Block[], typed: PMNode[]): { doc: PMNode; baseline: BaselineBlock[] } {
  const { nodes, baseline } = loadDoc({ docId: 'd', epoch: 1, blocks })
  return { doc: schema.node('doc', null, [...nodes, ...typed]), baseline }
}
const md = (s: string): PMNode[] => {
  const out: PMNode[] = []
  parser.parse(s)!.forEach((n) => out.push(n))
  return out
}

let n = 0
const newId = () => `new-${++n}`

/** a save that landed: stamp, rebuild the baseline */
function land(doc: PMNode, baseline: BaselineBlock[]) {
  const plan = planSave(doc, baseline, [], newId)
  const tr = EditorState.create({ schema, doc }).tr
  applyStamps(tr, plan.stamps)
  return { plan, doc: tr.doc, baseline: rebuildBaseline(baseline, plan.entries, plan.ops) }
}

const idOfText = (doc: PMNode, text: string): string | null => {
  let id: string | null = null
  doc.forEach((node) => {
    if (node.textContent === text) id = node.attrs.blockId
  })
  return id
}
const insertFor = (ops: Op[], content: string) => ops.find((o) => o.kind.op === 'insert' && o.kind.content === content)!.kind

describe('stamping new block ids after a save', () => {
  const blocks = [block('t', '# T', null, 'i'), block('a', '## A', 't', 'i'), block('x', 'text', 'a', 'i')]

  it('a paste spanning sections: every node gets its own insert id (inserts are grouped by parent)', () => {
    const { doc, baseline } = docWith(blocks, md('more\n\n## B\n\nbody'))
    const { plan, doc: after, baseline: next } = land(doc, baseline)
    // computeOps emits [B, more, body]: not doc order
    expect(plan.ops.filter((o) => o.kind.op === 'insert').map((o) => o.kind.content)).toEqual(['## B', 'more', 'body'])
    expect(idOfText(after, 'more')).toBe(insertFor(plan.ops, 'more').block_id)
    expect(idOfText(after, 'B')).toBe(insertFor(plan.ops, '## B').block_id)
    expect(idOfText(after, 'body')).toBe(insertFor(plan.ops, 'body').block_id)
    // the baseline agrees with the server: same id → same parent and key
    for (const c of ['more', '## B', 'body']) {
      const ins = insertFor(plan.ops, c)
      expect(next.find((b) => b.id === ins.block_id)).toMatchObject({ content: c, parent: ins.parent_id, order_key: ins.order_key })
    }
    // nothing more to send
    expect(planSave(after, next, [], newId).ops).toEqual([])
  })

  it('a later edit of the pasted paragraph replaces that paragraph, never the heading', () => {
    const { doc, baseline } = docWith(blocks, md('more\n\n## B\n\nbody'))
    const { plan, doc: after, baseline: next } = land(doc, baseline)
    // type into "more"
    let pos = -1
    after.forEach((node, p) => {
      if (node.textContent === 'more') pos = p
    })
    const state = EditorState.create({ schema, doc: after })
    const edited = state.apply(state.tr.insertText('!', pos + 1 + 'more'.length)).doc
    const ops = planSave(edited, next, [], newId).ops
    expect(ops).toHaveLength(1)
    expect(ops[0].kind).toMatchObject({ op: 'replace', target: insertFor(plan.ops, 'more').block_id, content: 'more!' })
  })

  it('a horizontal rule, an empty heading and an empty code block are stamped too, and shift nothing', () => {
    const empties = [
      schema.nodes.horizontalRule.create(),
      schema.nodes.heading.create({ level: 2 }),
      schema.nodes.codeBlock.create(),
      ...md('after'),
    ]
    const { doc, baseline } = docWith(blocks, empties)
    const { plan, doc: after, baseline: next } = land(doc, baseline)
    const ids: (string | null)[] = []
    after.forEach((node) => ids.push(node.attrs.blockId))
    expect(ids.every((id) => id != null)).toBe(true)
    expect(idOfText(after, 'after')).toBe(insertFor(plan.ops, 'after').block_id)
    expect(planSave(after, next, [], newId).ops).toEqual([])
  })

  it('mixed parents with new blocks between kept ones', () => {
    const { nodes, baseline } = loadDoc({ docId: 'd', epoch: 1, blocks })
    const doc = schema.node('doc', null, [...md('lead'), nodes[0], ...md('## Z\n\nunder z'), nodes[1], ...md('top of a'), nodes[2], ...md('tail')])
    const { plan, doc: after, baseline: next } = land(doc, baseline)
    for (const t of ['lead', 'under z', 'top of a', 'tail']) expect(idOfText(after, t)).toBe(insertFor(plan.ops, t).block_id)
    expect(idOfText(after, 'Z')).toBe(insertFor(plan.ops, '## Z').block_id)
    expect(planSave(after, next, [], newId).ops).toEqual([])
  })

  it('the ids go on before the request, so edits made while it is in flight keep them on their nodes', () => {
    const { doc, baseline } = docWith(blocks, md('more\n\n## B\n\nbody'))
    const plan = planSave(doc, baseline, [], newId)
    let state = EditorState.create({ schema, doc })
    const stamp = state.tr
    applyStamps(stamp, plan.stamps)
    state = state.apply(stamp)
    // meanwhile: a paragraph typed at the very top, and "body" deleted
    state = state.apply(state.tr.insert(0, schema.nodes.paragraph.create(null, schema.text('typed meanwhile'))))
    let bodyPos = -1
    state.doc.forEach((node, p) => {
      if (node.textContent === 'body') bodyPos = p
    })
    state = state.apply(state.tr.delete(bodyPos, bodyPos + state.doc.nodeAt(bodyPos)!.nodeSize))
    expect(idOfText(state.doc, 'more')).toBe(insertFor(plan.ops, 'more').block_id)
    expect(idOfText(state.doc, 'B')).toBe(insertFor(plan.ops, '## B').block_id)
    expect(idOfText(state.doc, 'typed meanwhile')).toBeNull()
    // the next save inserts the new paragraph and deletes "body"
    const nextOps = planSave(state.doc, rebuildBaseline(baseline, plan.entries, plan.ops), [], newId).ops
    expect(nextOps.map((o) => o.kind.op).sort()).toEqual(['delete', 'insert'])
    expect(nextOps.find((o) => o.kind.op === 'delete')!.kind.target).toBe(insertFor(plan.ops, 'body').block_id)
  })

  it('a failed save keeps the ids: its retry sends the very same inserts', () => {
    const { doc, baseline } = docWith(blocks, md('more\n\n## B'))
    const plan = planSave(doc, baseline, [], newId)
    const tr = EditorState.create({ schema, doc }).tr
    applyStamps(tr, plan.stamps)
    // the request failed: the baseline did not move
    const retry = planSave(tr.doc, baseline, [], newId)
    expect(retry.ops).toEqual(plan.ops)
    expect(retry.stamps).toEqual([])
  })

  it('entriesOf reports where each entry starts', () => {
    const { doc } = docWith(blocks, md('more'))
    const positions: number[] = []
    const entries = entriesOf(doc, positions)
    expect(positions).toHaveLength(entries.length)
    positions.forEach((p, i) => expect(doc.nodeAt(p)!.attrs.blockId).toBe(entries[i].id))
  })
})
