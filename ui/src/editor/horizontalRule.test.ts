// A horizontal rule is saved as `---`. Only a real YAML block (`---` line,
// then a closing `---` line) is frontmatter, and only the first block is
// the doc's frontmatter: a rule must stay visible in the editor, and "+ tag"
// must never write into one.
import { describe, expect, it } from 'vitest'
import type { Block, BlockNode } from '../types'
import { inferBlockType, isFrontmatter } from './diff'
import { entriesOf, loadDoc, planSave, schema } from './DocEditor'
import { splitTree } from './splitTree'

function block(id: string, content: string, order_key: string, block_type = 'paragraph'): Block {
  return { id, doc_id: 'd', parent_id: null, order_key, block_type, content, created_by: 'me', epoch: 1, deleted: false, refers_to: null }
}
const node = (b: Block, children: BlockNode[] = []): BlockNode => ({ block: b, children })

describe('horizontal rules', () => {
  it('only a closed YAML block is frontmatter (mirrors the store: import::is_frontmatter)', () => {
    expect(isFrontmatter('---\ntags: [a]\n---')).toBe(true)
    expect(isFrontmatter('---\r\ntags: [a]\r\n---')).toBe(true)
    expect(isFrontmatter('---')).toBe(false)
    expect(isFrontmatter('----')).toBe(false)
    expect(isFrontmatter('---\nnot closed')).toBe(false)
  })

  it('a rule is typed as the store types it (a paragraph), frontmatter as code', () => {
    expect(inferBlockType('---')).toBe('paragraph')
    expect(inferBlockType('---\ntags: [a]\n---')).toBe('code')
    expect(inferBlockType('```js\nx\n```')).toBe('code')
  })

  it('a rule stays in the editor; the first block, when YAML, is the frontmatter', () => {
    const roots = [node(block('p', 'para', 'i')), node(block('hr', '---', 'm', 'code')), node(block('q', 'after', 'r'))]
    const split = splitTree(roots)
    expect(split.blocks.map((b) => b.id)).toEqual(['p', 'hr', 'q'])
    expect(split.hidden).toEqual([])
    expect(split.frontmatter).toBeNull()
  })

  it('a doc whose first block is a rule has no frontmatter, so "+ tag" inserts one instead of replacing the rule', () => {
    const split = splitTree([node(block('hr', '---', 'i', 'code')), node(block('p', 'para', 'r'))])
    expect(split.frontmatter).toBeNull()
    expect(split.blocks.map((b) => b.id)).toEqual(['hr', 'p'])
  })

  it('real frontmatter is still hidden and found', () => {
    const split = splitTree([node(block('fm', '---\ntags: [a]\n---', 'i', 'code')), node(block('hr', '---', 'm')), node(block('p', 'para', 'r'))])
    expect(split.frontmatter?.id).toBe('fm')
    expect(split.hidden.map((h) => h.id)).toEqual(['fm'])
    expect(split.blocks.map((b) => b.id)).toEqual(['hr', 'p'])
  })

  it('a YAML-shaped block further down is not the frontmatter (and stays hidden, as before)', () => {
    const split = splitTree([node(block('p', 'para', 'i')), node(block('y', '---\ntags: [b]\n---', 'r', 'code'))])
    expect(split.frontmatter).toBeNull()
    expect(split.hidden.map((h) => h.id)).toEqual(['y'])
  })

  it('a rule round-trips: loaded as a rule, an untouched doc sends nothing', () => {
    const split = splitTree([node(block('p', 'para', 'i')), node(block('hr', '---', 'm', 'code'))])
    const { nodes, baseline } = loadDoc({ docId: 'd', epoch: 1, blocks: split.blocks })
    expect(nodes.map((n) => n.type.name)).toEqual(['paragraph', 'horizontalRule'])
    const doc = schema.node('doc', null, nodes)
    expect(planSave(doc, baseline, split.hidden).ops).toEqual([])
  })

  it('a new rule is inserted as a paragraph-typed `---`', () => {
    const split = splitTree([node(block('p', 'para', 'i'))])
    const { nodes, baseline } = loadDoc({ docId: 'd', epoch: 1, blocks: split.blocks })
    const doc = schema.node('doc', null, [...nodes, schema.nodes.horizontalRule.create()])
    expect(entriesOf(doc).map((e) => e.content)).toEqual(['para', '---'])
    const ops = planSave(doc, baseline, split.hidden).ops
    expect(ops).toHaveLength(1)
    expect(ops[0].kind).toMatchObject({ op: 'insert', content: '---', block_type: 'paragraph' })
  })
})
