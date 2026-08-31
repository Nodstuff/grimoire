// Canvas block (5.8): a tldraw embed whose scene JSON is the block's content —
// opaque-ish through the gate, versioned/merged/provenance'd like any block.
// No live sync in v1 (P2.5 is the shape-CRDT future).

import { useCallback, useRef } from 'react'
import { Tldraw, createShapeId, getSnapshot, loadSnapshot, toRichText, type Editor } from 'tldraw'
import 'tldraw/tldraw.css'
import { api, Block } from './types'

/** Agent-friendly declarative diagrams: agents emit this instead of raw
 * tldraw JSON (internal, versioned, fragile). Rendered into real shapes on
 * load; the first human save persists the full scene. */
interface KsDiagram {
  nodes: { id: string; label: string; x?: number; y?: number; color?: string }[]
  edges: { from: string; to: string; label?: string }[]
}

const NODE_W = 190
const NODE_H = 70
const GAP_X = 110
const GAP_Y = 60

/** Layered left-to-right layout for nodes without explicit positions. */
function layout(d: KsDiagram): Map<string, { x: number; y: number }> {
  const pos = new Map<string, { x: number; y: number }>()
  const indeg = new Map<string, number>()
  for (const n of d.nodes) indeg.set(n.id, 0)
  for (const e of d.edges) indeg.set(e.to, (indeg.get(e.to) ?? 0) + 1)
  // BFS depth from roots
  const depth = new Map<string, number>()
  let frontier = d.nodes.filter((n) => (indeg.get(n.id) ?? 0) === 0).map((n) => n.id)
  if (frontier.length === 0 && d.nodes.length > 0) frontier = [d.nodes[0].id]
  frontier.forEach((id) => depth.set(id, 0))
  let guard = 0
  while (frontier.length && guard++ < 100) {
    const next: string[] = []
    for (const e of d.edges) {
      if (frontier.includes(e.from) && !depth.has(e.to)) {
        depth.set(e.to, (depth.get(e.from) ?? 0) + 1)
        next.push(e.to)
      }
    }
    frontier = next
  }
  const perCol = new Map<number, number>()
  for (const n of d.nodes) {
    const col = depth.get(n.id) ?? 0
    const row = perCol.get(col) ?? 0
    perCol.set(col, row + 1)
    pos.set(n.id, {
      x: n.x ?? col * (NODE_W + GAP_X),
      y: n.y ?? row * (NODE_H + GAP_Y),
    })
  }
  return pos
}

function renderKsDiagram(editor: Editor, d: KsDiagram) {
  const pos = layout(d)
  for (const n of d.nodes) {
    const p = pos.get(n.id)!
    editor.createShape({
      id: createShapeId(),
      type: 'geo',
      x: p.x,
      y: p.y,
      props: {
        geo: 'rectangle',
        w: NODE_W,
        h: NODE_H,
        color: (n.color as never) ?? 'light-blue',
        fill: 'semi',
        size: 's',
        richText: toRichText(n.label),
      },
    })
  }
  for (const e of d.edges) {
    const a = pos.get(e.from)
    const b = pos.get(e.to)
    if (!a || !b) continue
    editor.createShape({
      type: 'arrow',
      x: 0,
      y: 0,
      props: {
        start: { x: a.x + NODE_W, y: a.y + NODE_H / 2 },
        end: { x: b.x, y: b.y + NODE_H / 2 },
        color: 'grey',
      },
    })
  }
  editor.selectNone()
  editor.zoomToFit()
}

export default function CanvasBlock({
  block,
  epoch,
  onSaved,
  full = false,
}: {
  block: Block
  epoch: number
  onSaved: () => void
  full?: boolean
}) {
  const epochRef = useRef(epoch)
  epochRef.current = epoch
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const onMount = useCallback(
    (editor: Editor) => {
      editor.user.updateUserPreferences({ colorScheme: 'dark' })
      // load the stored scene: full tldraw snapshot, or an agent's
      // declarative ks_diagram rendered into real shapes
      try {
        const parsed = JSON.parse(block.content)
        if (parsed && parsed.document) loadSnapshot(editor.store, parsed)
        else if (parsed && parsed.ks_diagram) renderKsDiagram(editor, parsed.ks_diagram)
      } catch {
        // empty/new canvas — fine
      }
      // debounced save through the gate as the human principal
      const unlisten = editor.store.listen(
        () => {
          if (timer.current) clearTimeout(timer.current)
          timer.current = setTimeout(async () => {
            const snapshot = getSnapshot(editor.store)
            try {
              const out = await api<{ epoch: number }>('/api/propose', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                  doc_id: block.doc_id,
                  base_epoch: epochRef.current,
                  ops: [
                    {
                      kind: {
                        op: 'replace',
                        target: block.id,
                        content: JSON.stringify(snapshot),
                      },
                      source_refs: ['canvas:edit'],
                    },
                  ],
                }),
              })
              epochRef.current = out.epoch
              onSaved()
            } catch (e) {
              console.error('canvas save failed', e)
            }
          }, 1500)
        },
        { scope: 'document', source: 'user' },
      )
      // tldraw unmounts the whole editor with the component; the listener
      // dies with the store. Keep the handle to satisfy the linter.
      void unlisten
    },
    [block.id, block.doc_id],
  )

  return (
    <div className={full ? 'canvas-full' : 'canvas-block'}>
      <Tldraw onMount={onMount} />
    </div>
  )
}
