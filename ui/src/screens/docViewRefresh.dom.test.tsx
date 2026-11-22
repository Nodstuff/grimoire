// @vitest-environment jsdom
// Someone else's edit landing while you type: the daemon rebases the save
// (base 5 answers epoch 7), so that epoch is not ours alone. The editor must
// reload once clean to show the other edit — counting 7 as our own hid it,
// and the next edit of that block then overwrote it.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act } from 'react'
import type { Editor } from '@tiptap/core'
import { advance } from '../editor/editorHarness'
import { fakeServer, makeTree, mountDocView } from './docViewHarness'
import { reloadVerdict } from './DocView'

beforeEach(() => vi.useFakeTimers())
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

/** type `text` at the end of the paragraph that reads `para` */
async function typeInto(ed: Editor, para: string, text: string) {
  await act(async () => {
    let at = -1
    ed.state.doc.forEach((n, p) => {
      if (n.textContent === para) at = p + 1 + para.length
    })
    ed.commands.insertContentAt(at, text)
  })
}

describe('an agent edit that lands while you type', () => {
  it('is shown once the editor is clean, and a later edit keeps it', async () => {
    const server = fakeServer(makeTree(5, [['a', 'alpha'], ['b', 'beta']]))
    const v = await mountDocView()
    await advance(10)
    const first = v.editor()!
    await typeInto(first, 'beta', ' typed')
    // meanwhile an agent rewrote "alpha" (epoch 6); our save rebases to 7
    server.propose = () => ({ doc_id: 'd', epoch: 7, verdicts: [] })
    server.tree = makeTree(7, [['a', 'alpha by agent'], ['b', 'beta typed']])
    await advance(1300)
    await advance(50)
    expect(v.text()).toContain('alpha by agent')
    expect(v.text()).toContain('beta typed')
    // the next edit of that block carries the agent's text
    server.propose = (b) => ({ doc_id: 'd', epoch: (b.base_epoch as number) + 1, verdicts: [] })
    await typeInto(v.editor()!, 'alpha by agent', '!')
    await advance(1300)
    const last = server.calls.filter((c) => c.path === '/api/propose').at(-1)!
    expect(last.body!.base_epoch).toBe(7)
    const ops = last.body!.ops as { kind: { op: string; target?: string; content?: string } }[]
    expect(ops.map((o) => o.kind)).toEqual([{ op: 'replace', target: 'a', content: 'alpha by agent!' }])
    await v.unmount()
  })

  it('a newer tree that arrives while typing waits, then reloads once the save lands', async () => {
    const server = fakeServer(makeTree(5, [['a', 'alpha'], ['b', 'beta']]))
    const v = await mountDocView()
    await advance(10)
    await typeInto(v.editor()!, 'beta', ' x')
    // a stamp tick while dirty: an agent's edit is on the server
    server.tree = makeTree(6, [['a', 'alpha 2'], ['b', 'beta']])
    await v.render(1)
    await advance(10)
    expect(v.text()).toContain('beta x') // the typing is untouched
    expect(v.text()).not.toContain('alpha 2')
    server.propose = () => ({ doc_id: 'd', epoch: 7, verdicts: [] })
    server.tree = makeTree(7, [['a', 'alpha 2'], ['b', 'beta x']])
    await advance(1300)
    await advance(50)
    expect(v.text()).toContain('alpha 2')
    expect(v.text()).toContain('beta x')
    await v.unmount()
  })

  it('our own save (base + 1) never remounts the editor', async () => {
    const server = fakeServer(makeTree(5, [['a', 'alpha']]))
    const v = await mountDocView()
    await advance(10)
    const first = v.editor()!
    await typeInto(first, 'alpha', ' more')
    server.tree = makeTree(6, [['a', 'alpha more']])
    await advance(1300)
    await v.render(1) // the stamp tick our save caused
    await advance(50)
    expect(v.editor()).toBe(first)
    await v.unmount()
  })

  it('a change only to hidden blocks (a comment) catches up without a remount', async () => {
    const server = fakeServer(makeTree(5, [['a', 'alpha']]))
    const v = await mountDocView()
    await advance(10)
    const first = v.editor()!
    const t = makeTree(6, [['a', 'alpha']])
    t.roots.push({ block: { ...t.roots[0].block, id: 'c', block_type: 'comment', content: 'nice', order_key: 'z', refers_to: 'a' }, children: [] })
    server.tree = t
    await v.render(1)
    await advance(50)
    expect(v.editor()).toBe(first)
    // and the next save is based on the new epoch
    await typeInto(first, 'alpha', '!')
    await advance(1300)
    expect(server.calls.filter((c) => c.path === '/api/propose').at(-1)!.body!.base_epoch).toBe(6)
    await v.unmount()
  })

  it('a remount keeps the cursor where it was', async () => {
    const server = fakeServer(makeTree(5, [['a', 'alpha'], ['b', 'beta']]))
    const v = await mountDocView()
    await advance(10)
    await act(async () => {
      v.editor()!.commands.setTextSelection(3)
    })
    server.tree = makeTree(6, [['a', 'alpha'], ['b', 'beta by agent']])
    await v.render(1)
    await advance(50)
    expect(v.text()).toContain('beta by agent')
    expect(v.editor()!.state.selection.anchor).toBe(3)
    await v.unmount()
  })
})

describe('reloadVerdict', () => {
  const base = { editorEpoch: 5, ownEpoch: 0, lastSaved: 0, behind: false, clean: true }
  it('nothing new: none', () => {
    expect(reloadVerdict({ ...base, next: 5 })).toBe('none')
    expect(reloadVerdict({ ...base, next: 7, ownEpoch: 7, lastSaved: 7 })).toBe('none')
  })
  it('newer and clean: remount; dirty: defer', () => {
    expect(reloadVerdict({ ...base, next: 6 })).toBe('remount')
    expect(reloadVerdict({ ...base, next: 6, clean: false })).toBe('defer')
  })
  it('behind (a rebased save): remount even at an epoch we produced', () => {
    expect(reloadVerdict({ ...base, next: 7, lastSaved: 7, behind: true })).toBe('remount')
  })
  it('a tree older than our latest save never replaces the editor', () => {
    expect(reloadVerdict({ ...base, next: 7, lastSaved: 8, behind: true })).toBe('defer')
  })
})

describe('another doc in the same DocView', () => {
  it('starts with no remount count: it never restores a cursor from an earlier visit', async () => {
    const server = fakeServer(makeTree(5, [['a', 'alpha beta'], ['b', 'beta']]))
    const e = makeTree(3, [['x', 'echo text']])
    e.doc = { ...e.doc, id: 'e' }
    server.others.e = e
    const v = await mountDocView(0)
    await v.render(0, 'e')
    await advance(10)
    await act(async () => {
      v.editor()!.commands.setTextSelection(4)
    })
    // to d, whose editor then remounts once (an agent's edit)
    await v.render(0, 'd')
    await advance(10)
    server.tree = makeTree(6, [['a', 'alpha beta'], ['b', 'beta by agent']])
    await v.render(1, 'd')
    await advance(50)
    expect(v.text()).toContain('beta by agent')
    // back to e: a fresh open, the cursor at the start
    await v.render(1, 'e')
    await advance(50)
    expect(v.text()).toContain('echo text')
    expect(v.editor()!.state.selection.anchor).toBe(1)
    await v.unmount()
  })
})

describe('the cursor across a remount', () => {
  const textOf = (ed: Editor, id: string) => {
    let t = ''
    ed.state.doc.forEach((n) => {
      if (n.attrs.blockId === id) t = n.textContent
    })
    return t
  }
  it('stays in its block when an agent lengthened and added blocks ABOVE it', async () => {
    const server = fakeServer(makeTree(5, [['a', 'alpha'], ['b', 'beta'], ['c', 'gamma']]))
    const v = await mountDocView()
    await advance(10)
    // the cursor after "ga" in gamma
    await act(async () => {
      const ed = v.editor()!
      let at = -1
      ed.state.doc.forEach((n, p) => {
        if (n.attrs.blockId === 'c') at = p + 1 + 2
      })
      ed.commands.setTextSelection(at)
    })
    server.tree = makeTree(6, [['a', 'alpha, now a much longer paragraph'], ['n', 'a whole new block'], ['b', 'beta'], ['c', 'gamma']])
    await v.render(1)
    await advance(50)
    expect(v.text()).toContain('a whole new block')
    const ed = v.editor()!
    await act(async () => {
      ed.commands.insertContent('X')
    })
    expect(textOf(ed, 'c')).toBe('gaXmma')
    expect(textOf(ed, 'a')).toBe('alpha, now a much longer paragraph')
    await v.unmount()
  })

  it('is not restored when its block is gone', async () => {
    const server = fakeServer(makeTree(5, [['a', 'alpha'], ['b', 'beta']]))
    const v = await mountDocView()
    await advance(10)
    await act(async () => {
      v.editor()!.commands.setTextSelection(1 + 'alpha'.length + 2 + 2)
    })
    server.tree = makeTree(6, [['a', 'alpha']])
    await v.render(1)
    await advance(50)
    expect(v.text()).not.toContain('beta')
    expect(v.editor()!.state.selection.anchor).toBe(1)
    await v.unmount()
  })
})
