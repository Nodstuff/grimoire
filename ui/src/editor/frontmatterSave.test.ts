// The web editor's save path with frontmatter present: App's tree split
// (splitTree) → DocEditor's load (loadDoc) → the editor doc → entries
// (entriesOf) → computeOps with the hidden blocks. Only the Tiptap view is
// left out: the editor doc is built from the loaded nodes directly.
import { describe, expect, it } from 'vitest'
import type { Block, BlockNode } from '../types'
import { computeOps } from './diff'
import { entriesOf, loadDoc, schema } from './DocEditor'
import { splitTree } from './splitTree'

function block(id: string, content: string, order_key: string, block_type = 'paragraph', parent_id: string | null = null): Block {
  return { id, doc_id: 'd', parent_id, order_key, block_type, content, created_by: 'me', epoch: 1, deleted: false, refers_to: null }
}
const node = (b: Block, children: BlockNode[] = []): BlockNode => ({ block: b, children })

describe('saving a doc with frontmatter', () => {
  const roots = [node(block('fm', '---\ntags: [a]\n---', 'i', 'code')), node(block('p', 'para', 'r'))]
  const split = splitTree(roots)
  const editable = { docId: 'd', epoch: 1, blocks: split.blocks, hidden: split.hidden }
  const { nodes, baseline } = loadDoc(editable)

  it('leaves the frontmatter out, in place', () => {
    expect(split.blocks.map((b) => b.id)).toEqual(['p'])
    expect(split.hidden.map((h) => [h.id, h.after])).toEqual([['fm', null]])
  })

  it('an untouched doc sends nothing', () => {
    const doc = schema.node('doc', null, nodes)
    expect(computeOps(baseline, entriesOf(doc), () => 'new', undefined, editable.hidden)).toEqual([])
  })

  it('a new first paragraph goes after the frontmatter', () => {
    const typed = schema.node('paragraph', null, [schema.text('top')])
    const doc = schema.node('doc', null, [typed, ...nodes])
    const ops = computeOps(baseline, entriesOf(doc), () => 'new', undefined, editable.hidden)
    expect(ops).toHaveLength(1)
    const k = ops[0].kind as { op: string; parent_id: string | null; order_key: string }
    expect(k.op).toBe('insert')
    expect(k.parent_id).toBeNull()
    expect(k.order_key > 'i' && k.order_key < 'r').toBe(true)
  })
})
