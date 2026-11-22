// @vitest-environment jsdom
// Review decorations after an edit: mapped when the edit touches no decorated
// block, rebuilt when it does — and either way exactly what a full rebuild
// gives. The word diff behind a yellow block runs once per (before, now).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { Editor } from '@tiptap/core'
import type { DecorationSet } from '@tiptap/pm/view'
import { makeExtensions } from './DocEditor'
import type { ReviewDetail } from './ReviewHighlight'

const calls = { n: 0 }
vi.mock('./docMeta', async (orig) => {
  const m = await orig<typeof import('./docMeta')>()
  return {
    ...m,
    wordDiff: (a: string, b: string) => {
      calls.n++
      return m.wordDiff(a, b)
    },
  }
})
const { buildDecos, reviewKey, setReviewMap, touchesDecorated } = await import('./ReviewHighlight')

const detail = (id: string, tone: 'yellow' | 'red', beforeText: string | null = null): ReviewDetail => ({
  annotationId: `ann-${id}`,
  tone,
  who: 'claude:t',
  agent: true,
  confidence: 0.7,
  source: null,
  beforeText,
  proposedText: tone === 'red' ? 'proposed' : null,
  accept: 'keep',
  decline: 'revert',
  canResolve: true,
})
const map = { y: 'yellow', r: 'red', m: 'yellow' } as const
const extras = {
  details: { y: detail('y', 'yellow', 'the human wrote text'), r: detail('r', 'red'), m: detail('m', 'yellow', 'first') },
  comments: { c: 2, y: 1 },
}

const para = (id: string | null, text: string) => ({ type: 'paragraph', attrs: { blockId: id }, content: text ? [{ type: 'text', text }] : [] })

let editor: Editor
beforeEach(() => {
  editor = new Editor({
    element: document.createElement('div'),
    extensions: makeExtensions(),
    content: {
      type: 'doc',
      content: [
        para('i', 'intro'),
        para('y', 'the agent wrote this text'),
        para('r', 'red one'),
        para('c', 'commented'),
        para('m', 'first part'),
        para('m', 'second part'),
        para('t', 'tail'),
      ],
    },
  })
  setReviewMap(editor, { ...map }, extras)
})
afterEach(() => editor.destroy())

function norm(set: DecorationSet) {
  return set
    .find()
    .map((d) => {
      const t = (d as unknown as { type: { attrs?: Record<string, string> } }).type
      return `${d.from}-${d.to} ${d.spec.key ?? ''} ${JSON.stringify(t.attrs ?? {})}`
    })
    .sort()
}
const current = () => norm(reviewKey.getState(editor.state)!.decos)
const rebuilt = () => norm(buildDecos(editor.state, { ...map }, extras))
/** where the node with this block id starts (`last`: its last node) */
const at = (id: string, last = false) => {
  let found = -1
  editor.state.doc.forEach((n, p) => {
    if (n.attrs.blockId === id && (last || found < 0)) found = p
  })
  return found
}

describe('review decorations follow edits', () => {
  const edits: [string, () => void][] = [
    ['type in an undecorated block', () => editor.commands.insertContentAt(at('i') + 3, 'XYZ')],
    ['type at the end of the doc', () => editor.commands.insertContentAt(editor.state.doc.content.size - 1, ' end')],
    ['type in the yellow block', () => editor.commands.insertContentAt(at('y') + 5, 'new ')],
    ['type in the commented block', () => editor.commands.insertContentAt(at('c') + 2, 'zz')],
    ['a new paragraph between two blocks', () => editor.commands.insertContentAt(at('r'), para(null, 'between'))],
    ['a new paragraph right after the multi-node block', () => editor.commands.insertContentAt(at('t'), para(null, 'after m'))],
    [
      'delete an undecorated block',
      () => {
        const p = at('t')
        editor.view.dispatch(editor.state.tr.delete(p, p + editor.state.doc.nodeAt(p)!.nodeSize))
      },
    ],
    [
      'delete the last node of a multi-node block (its bar moves up)',
      () => {
        const p = at('m', true)
        editor.view.dispatch(editor.state.tr.delete(p, p + editor.state.doc.nodeAt(p)!.nodeSize))
      },
    ],
    ['paste a node carrying a decorated block id elsewhere', () => editor.commands.insertContentAt(editor.state.doc.content.size, para('m', 'pasted m'))],
    [
      "stamp a new block's id",
      () => {
        editor.commands.insertContentAt(at('i'), para(null, 'fresh'))
        let p = -1
        editor.state.doc.forEach((n, q) => {
          if (n.textContent === 'fresh') p = q
        })
        editor.view.dispatch(editor.state.tr.setNodeAttribute(p, 'blockId', 'fresh-id'))
      },
    ],
    ['split the yellow block', () => editor.chain().setTextSelection(at('y') + 4).splitBlock().run()],
    [
      'join an undecorated block into a decorated one',
      () => {
        const p = at('c')
        const end = p + editor.state.doc.nodeAt(p)!.nodeSize
        editor.view.dispatch(editor.state.tr.join(end))
      },
    ],
  ]
  for (const [name, edit] of edits) {
    it(`${name}: the same decorations as a full rebuild`, () => {
      edit()
      expect(current()).toEqual(rebuilt())
    })
  }

  it('a run of mixed edits stays equal to a rebuild after each', () => {
    for (const [, edit] of edits.slice(0, 6)) {
      edit()
      expect(current()).toEqual(rebuilt())
    }
  })
})

describe('word diffs are cached', () => {
  it('typing outside the yellow block re-diffs nothing; typing in it diffs once per change', () => {
    calls.n = 0
    for (let i = 0; i < 20; i++) editor.commands.insertContentAt(at('i') + 1, 'a')
    expect(calls.n).toBe(0)
    editor.commands.insertContentAt(at('y') + 1, 'Q')
    expect(calls.n).toBe(1)
    // typing in the commented block (a rebuild) reuses the yellow block's diff
    editor.commands.insertContentAt(at('c') + 1, 'k')
    expect(calls.n).toBe(1)
  })
})

describe('touchesDecorated', () => {
  const touches = (make: () => import('@tiptap/pm/state').Transaction) => touchesDecorated(make(), { ...map }, extras.comments)
  it('typing in a plain block far from any decorated one: no (the decorations are only mapped)', () => {
    expect(touches(() => editor.state.tr.insertText('x', at('t') + 2))).toBe(false)
  })
  it('typing in, beside or over a decorated block: yes (rebuilt)', () => {
    expect(touches(() => editor.state.tr.insertText('x', at('y') + 2))).toBe(true)
    expect(touches(() => editor.state.tr.insertText('x', at('c') + 2))).toBe(true)
    // the intro sits right before the yellow block: an insert at its edge counts
    expect(touches(() => editor.state.tr.insert(at('y'), editor.schema.nodes.paragraph.create()))).toBe(true)
  })
  it("a block id stamp is not a touch", () => {
    expect(touches(() => editor.state.tr.setNodeAttribute(at('t'), 'blockId', 'new'))).toBe(false)
  })
})
