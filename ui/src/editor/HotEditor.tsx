// Hot session editor (#65, ADR 0003): the doc is LIVE — a Yjs doc hosted by
// the daemon is the source of truth, synced over /ws/hot/{doc}. The normal
// autosave machinery is off; the epoch is frozen. Whoever ends the session
// flattens the final state through the same diff/propose path the cold
// editor uses (block ids ride along as node attrs, so unchanged blocks keep
// their identity and comment anchors survive).

import { useEffect, useMemo, useRef, useState } from 'react'
import { EditorContent, useEditor } from '@tiptap/react'
import Collaboration from '@tiptap/extension-collaboration'
import * as Y from 'yjs'
import { WebsocketProvider } from 'y-websocket'
import { prosemirrorJSONToYDoc } from 'y-prosemirror'
import type { Node as PMNode } from '@tiptap/pm/model'
import { api, Block } from '../types'
import { BaselineBlock, Entry, computeOps } from './diff'
import { extensions, parser, schema, serializer } from './DocEditor'
import { nodesToMarkdown } from './markdown'

export interface HotDoc {
  docId: string
  frozenEpoch: number
  /** this client created the session and must seed it from current content */
  seed: boolean
  blocks: Block[]
}

export default function HotEditor({
  doc,
  onEnded,
}: {
  doc: HotDoc
  /** session over (we ended it, or the daemon dropped it) — reload cold */
  onEnded: () => void
}) {
  const [peers, setPeers] = useState(1)
  const [connected, setConnected] = useState(false)
  const [ending, setEnding] = useState(false)
  const endedRef = useRef(false)

  // parse current blocks exactly like the cold editor: nodes + baseline
  const initial = useMemo(() => {
    const nodes: PMNode[] = []
    const baseline: BaselineBlock[] = []
    for (const b of doc.blocks) {
      const parsed = parser.parse(b.content)
      if (!parsed || parsed.childCount === 0) continue
      const children: PMNode[] = []
      parsed.forEach((child) => {
        children.push(child.type.create({ ...child.attrs, blockId: b.id }, child.content, child.marks))
      })
      nodes.push(...children)
      baseline.push({
        id: b.id,
        content: nodesToMarkdown(serializer, schema, children),
        parent: b.parent_id,
        order_key: b.order_key,
      })
    }
    return { nodes, baseline }
  }, [doc.docId])

  const { ydoc, provider } = useMemo(() => {
    const ydoc = new Y.Doc()
    if (doc.seed) {
      const pmJson = { type: 'doc', content: initial.nodes.map((n) => n.toJSON()) }
      const seeded = prosemirrorJSONToYDoc(schema, pmJson, 'default')
      Y.applyUpdate(ydoc, Y.encodeStateAsUpdate(seeded))
    }
    const proto = location.protocol === 'https:' ? 'wss' : 'ws'
    const provider = new WebsocketProvider(`${proto}://${location.host}/ws/hot`, doc.docId, ydoc)
    return { ydoc, provider }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [doc.docId])

  useEffect(() => {
    const onStatus = ({ status }: { status: string }) => setConnected(status === 'connected')
    const onAwareness = () => setPeers(provider.awareness.getStates().size)
    provider.on('status', onStatus)
    provider.awareness.on('change', onAwareness)
    // daemon dropped the session (someone else ended it): the provider just
    // retries forever, so ask the daemon whether the doc is still hot
    const onClose = () => {
      if (endedRef.current) return
      api<{ hot: boolean }>(`/api/doc/${doc.docId}/hot/status`)
        .then((st) => {
          if (!st.hot && !endedRef.current) {
            endedRef.current = true
            onEnded()
          }
        })
        .catch(() => {})
    }
    provider.on('connection-close', onClose)
    return () => {
      provider.off('status', onStatus)
      provider.off('connection-close', onClose)
      provider.awareness.off('change', onAwareness)
      provider.destroy()
      ydoc.destroy()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [provider])

  const editor = useEditor(
    {
      extensions: [
        ...extensions.filter((e) => e.name !== 'undoRedo'), // Yjs owns history
        Collaboration.configure({ document: ydoc, field: 'default' }),
      ],
      // content comes exclusively from the Yjs doc
    },
    [doc.docId],
  )

  // the ender flattens: final editor state → entries → ops vs the frozen
  // baseline → one propose at the frozen epoch → confirm drops the journal
  const endSession = async () => {
    if (!editor || ending) return
    setEnding(true)
    endedRef.current = true
    try {
      await api(`/api/doc/${doc.docId}/hot/end`, { method: 'POST' })
      const entries: Entry[] = []
      let run: { id: string; nodes: PMNode[] } | null = null
      const flush = () => {
        if (run) {
          entries.push({
            id: run.id,
            content: nodesToMarkdown(serializer, schema, run.nodes),
            level: run.nodes[0].type.name === 'heading' ? run.nodes[0].attrs.level : 0,
          })
          run = null
        }
      }
      editor.state.doc.forEach((node) => {
        if (node.type.name === 'paragraph' && node.content.size === 0 && !node.attrs.blockId) return
        const id: string | null = node.attrs.blockId ?? null
        if (id && run && run.id === id) {
          run.nodes.push(node)
          return
        }
        flush()
        if (id) run = { id, nodes: [node] }
        else
          entries.push({
            id: null,
            content: nodesToMarkdown(serializer, schema, [node]),
            level: node.type.name === 'heading' ? node.attrs.level : 0,
          })
      })
      flush()
      const ops = computeOps(initial.baseline, entries, () => crypto.randomUUID())
      if (ops.length > 0) {
        await api('/api/propose', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            doc_id: doc.docId,
            base_epoch: doc.frozenEpoch,
            ops,
          }),
        })
      }
      await api(`/api/doc/${doc.docId}/hot/confirm`, { method: 'POST' })
    } catch (e) {
      alert(String(e))
    }
    onEnded()
  }

  return (
    <>
      <div className="hot-banner">
        <span className="hot-dot" />
        live session · {peers} here · {connected ? 'synced' : 'connecting…'}
        <button className="hot-end" disabled={ending} onClick={endSession}>
          {ending ? 'saving…' : 'end session'}
        </button>
      </div>
      <EditorContent editor={editor} />
    </>
  )
}
