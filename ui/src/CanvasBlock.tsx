// Canvas block (5.8): a tldraw embed whose scene JSON is the block's content —
// opaque-ish through the gate, versioned/merged/provenance'd like any block.
// No live sync in v1 (P2.5 is the shape-CRDT future).

import { useCallback, useRef } from 'react'
import { Tldraw, getSnapshot, loadSnapshot, type Editor } from 'tldraw'
import 'tldraw/tldraw.css'
import { api, Block } from './types'

export default function CanvasBlock({
  block,
  epoch,
  onSaved,
}: {
  block: Block
  epoch: number
  onSaved: () => void
}) {
  const epochRef = useRef(epoch)
  epochRef.current = epoch
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const onMount = useCallback(
    (editor: Editor) => {
      editor.user.updateUserPreferences({ colorScheme: 'dark' })
      // load the stored scene, if any
      try {
        const parsed = JSON.parse(block.content)
        if (parsed && parsed.document) loadSnapshot(editor.store, parsed)
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
    <div className="canvas-block">
      <Tldraw onMount={onMount} />
    </div>
  )
}
