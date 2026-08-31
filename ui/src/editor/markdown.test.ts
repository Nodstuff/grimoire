import { describe, expect, it } from 'vitest'
import { getSchema } from '@tiptap/core'
import StarterKit from '@tiptap/starter-kit'
import CodeBlock from '@tiptap/extension-code-block'
import { TableKit } from '@tiptap/extension-table'
import { makeParser, makeSerializer, nodesToMarkdown } from './markdown'

const schema = getSchema([
  StarterKit.configure({ codeBlock: false }),
  CodeBlock,
  TableKit.configure({ table: { resizable: false } }),
])
const parser = makeParser(schema)
const serializer = makeSerializer()

function roundTrip(md: string): string {
  const doc = parser.parse(md)!
  const nodes: import('@tiptap/pm/model').Node[] = []
  doc.forEach((n) => nodes.push(n))
  return nodesToMarkdown(serializer, schema, nodes)
}

describe('tables', () => {
  it('parses a GFM table into table nodes and serializes back to pipes', () => {
    const md = '| Layer | Owns |\n|---|---|\n| Agent | tool-use loop |\n| Renderer | pixels |'
    const doc = parser.parse(md)!
    expect(doc.firstChild!.type.name).toBe('table')
    const out = roundTrip(md)
    expect(out).toContain('| Layer | Owns |')
    expect(out).toContain('| --- | --- |')
    expect(out).toContain('| Agent | tool-use loop |')
    // and the round-trip is stable
    expect(roundTrip(out)).toBe(out)
  })

  it('escapes pipes in cell text', () => {
    const md = '| a | b |\n|---|---|\n| x \\| y | z |'
    const out = roundTrip(md)
    expect(roundTrip(out)).toBe(out)
  })

  it('strikethrough survives', () => {
    expect(roundTrip('~~gone~~')).toBe('~~gone~~')
  })

  it('plain blocks unaffected by preset change', () => {
    expect(roundTrip('# Title\n\npara with **bold**')).toBe('# Title\n\npara with **bold**')
  })
})
