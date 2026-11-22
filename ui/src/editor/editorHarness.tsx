// Test-only: mount the real DocEditor (Tiptap included) in jsdom against a
// scripted fetch, and reach its Tiptap editor to type into it.
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { vi } from 'vitest'
import type { Editor } from '@tiptap/core'
import DocEditor, { type EditableDoc, type EditorSaveState } from './DocEditor'
import type { Block } from '../types'

;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

export function block(id: string, content: string, order_key = 'i', parent_id: string | null = null): Block {
  return { id, doc_id: 'd', parent_id, order_key, block_type: 'paragraph', content, created_by: 'me', epoch: 1, deleted: false, refers_to: null }
}

export interface Call {
  path: string
  body: Record<string, unknown> | null
  init: RequestInit
}

/** fetch answered by `reply` (an object = 200 JSON; an Error = network failure) */
export function scriptFetch(reply: (c: Call, n: number) => unknown): Call[] {
  const calls: Call[] = []
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init: RequestInit = {}) => {
      const c: Call = { path, body: init.body ? JSON.parse(String(init.body)) : null, init }
      calls.push(c)
      const r = reply(c, calls.length)
      if (r instanceof Error) throw r
      return new Response(JSON.stringify(r), { status: 200, headers: { 'Content-Type': 'application/json' } })
    }),
  )
  return calls
}

export interface Mounted {
  el: HTMLElement
  root: Root
  editor: () => Editor
  states: EditorSaveState[]
  saved: { epoch: number; docId: string; own: boolean }[]
  unmount: () => Promise<void>
}

export async function mountEditor(doc: EditableDoc, extra: Partial<Parameters<typeof DocEditor>[0]> = {}): Promise<Mounted> {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const root = createRoot(el)
  const states: EditorSaveState[] = []
  const saved: Mounted['saved'] = []
  await act(async () => {
    root.render(
      <DocEditor
        doc={doc}
        onSaveState={(s) => states.push(s)}
        onSaved={(epoch: number, docId: string, own?: boolean) => saved.push({ epoch, docId, own: own ?? true })}
        {...extra}
      />,
    )
  })
  const editor = () => (el.querySelector('.ProseMirror') as unknown as { editor: Editor }).editor
  return {
    el,
    root,
    editor,
    states,
    saved,
    unmount: async () => {
      await act(async () => root.unmount())
      el.remove()
    },
  }
}

/** type at the end of the doc */
export async function typeAtEnd(m: Mounted, text: string) {
  await act(async () => {
    const ed = m.editor()
    ed.commands.insertContentAt(ed.state.doc.content.size - 1, text)
  })
}

/** let timers and promises run */
export async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms)
  })
}
