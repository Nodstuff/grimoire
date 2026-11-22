// @vitest-environment jsdom
// A paste keeps the source's data-block-id. Copied from this doc, two nodes
// shared one id; from another doc, the insert went out under the other
// doc's id and was refused ("already exists"), parked red, and the clean
// editor then remounted without the paste.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act } from 'react'
import { advance, block, mountEditor, scriptFetch, type Call } from './editorHarness'

beforeEach(() => {
  vi.useFakeTimers()
  // jsdom has no ClipboardEvent; ProseMirror's pasteHTML makes one
  if (!('ClipboardEvent' in globalThis))
    vi.stubGlobal(
      'ClipboardEvent',
      class extends Event {
        clipboardData = null
      },
    )
})
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

const doc = { docId: 'd', epoch: 1, blocks: [block('a', 'alpha'), block('b', 'beta', 'r')] }
const ids = (ed: import('@tiptap/core').Editor) => {
  const out: (string | null)[] = []
  ed.state.doc.forEach((n) => out.push(n.attrs.blockId))
  return out
}
const proposes = (calls: Call[]) => calls.filter((c) => c.path === '/api/propose')
/** paste three paragraphs into a new last paragraph: the middle one arrives
 * whole, attributes and all (the outer two merge into the paragraph edges) */
function pasteMiddle(ed: import('@tiptap/core').Editor, middle: string) {
  ed.commands.insertContentAt(ed.state.doc.content.size, { type: 'paragraph' })
  ed.commands.setTextSelection(ed.state.doc.content.size - 1)
  ed.view.pasteHTML(`<p>pre</p>${middle}<p>post</p>`)
}
type Ops = { kind: { op: string; block_id?: string; content?: string } }[]

describe('pasting blocks', () => {
  it('a copy of a block in this doc arrives without its id and is saved as a new block', async () => {
    const calls = scriptFetch(() => ({ epoch: 2 }))
    const m = await mountEditor(doc)
    await act(async () => pasteMiddle(m.editor(), '<p data-block-id="a">alpha again</p>'))
    expect(m.editor().state.doc.textContent).toContain('alpha again')
    expect(ids(m.editor()).filter((x) => x === 'a')).toHaveLength(1)
    await advance(1300)
    const ops = proposes(calls)[0].body!.ops as Ops
    expect(ops.every((o) => o.kind.op === 'insert')).toBe(true)
    expect(ops.find((o) => o.kind.content === 'alpha again')!.kind.block_id).not.toBe('a')
    await m.unmount()
  })

  it("a block from another doc is inserted under a fresh id and stays", async () => {
    const calls = scriptFetch(() => ({ epoch: 2 }))
    const m = await mountEditor(doc)
    await act(async () => pasteMiddle(m.editor(), '<p data-block-id="0193aaaa-0000-7000-8000-000000000001">from elsewhere</p>'))
    expect(ids(m.editor())).toContain('0193aaaa-0000-7000-8000-000000000001')
    await advance(1300)
    const ops = proposes(calls)[0].body!.ops as Ops
    const ins = ops.find((o) => o.kind.content === 'from elsewhere')!
    expect(ins.kind.op).toBe('insert')
    expect(ins.kind.block_id).not.toBe('0193aaaa-0000-7000-8000-000000000001')
    expect(ids(m.editor())).toContain(ins.kind.block_id)
    expect(ids(m.editor())).not.toContain('0193aaaa-0000-7000-8000-000000000001')
    expect(m.states.at(-1)).toBe('clean')
    await m.unmount()
  })
})
