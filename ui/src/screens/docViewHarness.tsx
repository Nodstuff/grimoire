// Test-only: mount the real DocView (and its DocEditor) in jsdom against a
// scripted daemon: the doc tree is whatever `server.tree` holds when asked.
import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { vi } from 'vitest'
import type { Editor } from '@tiptap/core'
import DocView from './DocView'
import type { BlockNode, DocTree, QueueRow } from '../types'
import type { Panel } from './DocView'
import { block } from '../editor/editorHarness'

export interface FakeServer {
  tree: DocTree
  /** answer to a POST /api/propose (default: next epoch, applied) */
  propose: (body: Record<string, unknown>) => unknown
  /** answer to a POST /api/resolve (default: ok) */
  resolve: (body: Record<string, unknown>) => unknown
  review: QueueRow[]
  calls: { path: string; method: string; body: Record<string, unknown> | null }[]
}

export function makeTree(epoch: number, contents: [string, string][]): DocTree {
  const roots: BlockNode[] = contents.map(([id, c], i) => ({ block: { ...block(id, c, String.fromCharCode(105 + i)), epoch }, children: [] }))
  return {
    doc: { id: 'd', parent_id: null, title: 'T', review_policy: null, current_epoch: epoch, created_by: 'me', status: null, sort_key: null },
    roots,
  }
}

export function fakeServer(tree: DocTree): FakeServer {
  const server: FakeServer = {
    tree,
    propose: (body) => ({ doc_id: 'd', epoch: (body.base_epoch as number) + 1, verdicts: [] }),
    resolve: () => ({ ok: true }),
    review: [],
    calls: [],
  }
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init: RequestInit = {}) => {
      const method = (init.method ?? 'GET').toUpperCase()
      const body = init.body ? JSON.parse(String(init.body)) : null
      server.calls.push({ path, method, body })
      let r: unknown = []
      if (path === '/api/doc/d') r = server.tree
      else if (path === '/api/propose') r = server.propose(body)
      else if (path === '/api/resolve') r = server.resolve(body)
      else if (path === '/api/doc/d/review') r = server.review
      else if (path === '/api/profile') r = { principal_id: 'me', name: 'Me', confirmed: true }
      else if (path === '/api/workspaces') r = { workspaces: [] }
      else if (method !== 'GET') r = { ok: true }
      return new Response(JSON.stringify(r), { status: 200, headers: { 'Content-Type': 'application/json' } })
    }),
  )
  return server
}

export async function mountDocView(dataVersion = 0, panel: Panel = 'closed') {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const root = createRoot(el)
  const render = async (dv: number) =>
    act(async () => {
      root.render(
        <DocView docId="d" onOpenDoc={() => {}} docs={[]} dataVersion={dv} panel={panel} setPanel={() => {}} />,
      )
    })
  await render(dataVersion)
  return {
    el,
    render,
    editor: () => (el.querySelector('.ProseMirror') as unknown as { editor: Editor } | null)?.editor ?? null,
    text: () => el.querySelector('.ProseMirror')?.textContent ?? '',
    unmount: async () => {
      await act(async () => root.unmount())
      el.remove()
    },
  }
}

/** a yellow (applied, flagged) replace of `target` by an agent */
export function yellowReplace(id: string, target: string, before: string, after: string): QueueRow {
  return {
    item: {
      annotation: { id, doc_id: 'd', op_id: `op-${id}`, kind: 'review', status: 'open' },
      op: {
        id: `op-${id}`,
        kind: { op: 'replace', target, content: after },
        principal: 'agent',
        base_epoch: 4,
        epoch_applied: 5,
        verdict: 'yellow',
        confidence: 0.6,
        prior: { ...block(target, before), epoch: 4 },
        source_refs: [],
      },
    },
    doc_title: 'T',
    proposer: 'claude:test',
    current_content: after,
  }
}
