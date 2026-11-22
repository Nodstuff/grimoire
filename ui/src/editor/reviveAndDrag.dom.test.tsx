// @vitest-environment jsdom
// Two ways a block comes back under its own id: ⌘Z of a delete that already
// saved (the store revives a same-doc tombstone under its id, links and
// comment anchors included), and a drag that moves blocks within the doc
// (prosemirror-view transforms the dragged slice before deleting its source,
// so its ids still look present). Neither may become a new block.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act } from 'react'
import { Fragment, Slice } from '@tiptap/pm/model'
import type { EditorView } from '@tiptap/pm/view'
import { advance, block, mountEditor, scriptFetch, type Call } from './editorHarness'

beforeEach(() => vi.useFakeTimers())
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

const doc = { docId: 'd', epoch: 1, blocks: [block('a', 'alpha'), block('b', 'beta', 'r')] }
type Ops = { kind: { op: string; block_id?: string; target?: string } }[]
const proposes = (calls: Call[]) => calls.filter((c) => c.path === '/api/propose')

describe('a deleted block brought back', () => {
  it('⌘Z of a saved delete inserts it under its own id', async () => {
    let epoch = 1
    const calls = scriptFetch(() => ({ epoch: ++epoch }))
    const m = await mountEditor(doc)
    await act(async () => {
      const ed = m.editor()
      let at = -1
      ed.state.doc.forEach((n, p) => {
        if (n.attrs.blockId === 'b') at = p
      })
      ed.view.dispatch(ed.state.tr.delete(at, at + ed.state.doc.nodeAt(at)!.nodeSize))
    })
    await advance(1300)
    expect((proposes(calls)[0].body!.ops as Ops).map((o) => o.kind)).toEqual([{ op: 'delete', target: 'b' }])
    await act(async () => {
      m.editor().commands.undo()
    })
    await advance(1300)
    const ops = proposes(calls)[1].body!.ops as Ops
    expect(ops).toHaveLength(1)
    expect(ops[0].kind).toMatchObject({ op: 'insert', block_id: 'b' })
    await m.unmount()
  })
})

describe('a drag within the doc', () => {
  /** the slice transformPasted makes of a dragged copy of block "a" */
  function transformed(view: EditorView, dragging: { move: boolean } | null): (string | null)[] {
    const a = view.state.doc.child(0)
    let slice = new Slice(Fragment.from(a), 0, 0)
    ;(view as unknown as { dragging: unknown }).dragging = dragging ? { slice, move: dragging.move } : null
    view.someProp('transformPasted', (f) => {
      slice = f(slice, view, false)
    })
    ;(view as unknown as { dragging: unknown }).dragging = null
    const ids: (string | null)[] = []
    slice.content.forEach((n) => ids.push(n.attrs.blockId))
    return ids
  }

  it('a move keeps its blocks ids; a copy-drag and a paste get none', async () => {
    scriptFetch(() => ({ epoch: 2 }))
    const m = await mountEditor(doc)
    const view = m.editor().view
    expect(transformed(view, { move: true })).toEqual(['a'])
    expect(transformed(view, { move: false })).toEqual([null])
    expect(transformed(view, null)).toEqual([null])
    await m.unmount()
  })
})
