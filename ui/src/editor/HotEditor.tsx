// Hot session editor (#65, ADR 0003): the doc is LIVE — a Yjs doc hosted by
// the daemon is the source of truth, synced over /ws/hot/{doc}. The normal
// autosave machinery is off; the epoch is frozen. Whoever ends the session
// flattens the final state through the same diff/propose path the cold
// editor uses (block ids ride along as node attrs, so unchanged blocks keep
// their identity and comment anchors survive).

import { useEffect, useMemo, useRef, useState } from 'react'
import { EditorContent, useEditor } from '@tiptap/react'
import Collaboration from '@tiptap/extension-collaboration'
import CollaborationCaret from '@tiptap/extension-collaboration-caret'
import * as Y from 'yjs'
import { WebsocketProvider } from 'y-websocket'
import { prosemirrorJSONToYDoc } from 'y-prosemirror'
import type { Node as PMNode } from '@tiptap/pm/model'
import { api, Block, Principal } from '../types'
import { extensions, parser, schema } from './DocEditor'

const CARET_COLORS = ['#8b9dc3', '#95c99b', '#d9b47a', '#d98a94', '#a88bd4', '#7bc4c4']
function colorFor(name: string): string {
  let h = 0
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0
  return CARET_COLORS[h % CARET_COLORS.length]
}

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
  const [peerNames, setPeerNames] = useState<string[]>([])
  const [me, setMe] = useState<string>('me')
  const [connected, setConnected] = useState(false)
  const [ending, setEnding] = useState(false)
  const endedRef = useRef(false)

  // parse current blocks exactly like the cold editor (seeding only — the
  // flatten is the daemon's job now, #67)
  const initial = useMemo(() => {
    const nodes: PMNode[] = []
    for (const b of doc.blocks) {
      const parsed = parser.parse(b.content)
      if (!parsed || parsed.childCount === 0) continue
      parsed.forEach((child) => {
        nodes.push(child.type.create({ ...child.attrs, blockId: b.id }, child.content, child.marks))
      })
    }
    return { nodes }
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

  // identify ourselves in awareness: petname + stable color drive the carets
  useEffect(() => {
    api<Principal[]>('/api/principals')
      .then((ps) => {
        const human = ps.find((p) => p.kind === 'human')
        if (human) setMe(human.display_name)
      })
      .catch(() => {})
  }, [])
  useEffect(() => {
    provider.awareness.setLocalStateField('user', { name: me, color: colorFor(me) })
  }, [provider, me])

  useEffect(() => {
    const onStatus = ({ status }: { status: string }) => setConnected(status === 'connected')
    const onAwareness = () => {
      const names: string[] = []
      provider.awareness.getStates().forEach((state, clientId) => {
        if (clientId === provider.awareness.clientID) return
        const n = (state as { user?: { name?: string } }).user?.name
        names.push(n || 'someone')
      })
      setPeerNames(names)
    }
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
        CollaborationCaret.configure({
          provider,
          user: { name: me, color: colorFor(me) },
        }),
      ],
      // content comes exclusively from the Yjs doc
    },
    [doc.docId, me],
  )

  // ending is one call: the daemon renders the Yjs doc to markdown, diffs
  // it against the blocks (mddiff), lands one commit at the frozen epoch,
  // and drops the session + journal (#67)
  const endSession = async () => {
    if (ending) return
    setEnding(true)
    endedRef.current = true
    try {
      await api(`/api/doc/${doc.docId}/hot/end`, { method: 'POST' })
    } catch (e) {
      alert(String(e))
    }
    onEnded()
  }

  return (
    <>
      <div className="hot-banner">
        <span className="hot-dot" />
        live
        {peerNames.length > 0
          ? ` with ${peerNames.join(', ')}`
          : ' — waiting for others'}
        {' · '}
        {connected ? 'synced' : 'connecting…'}
        <button className="hot-end" disabled={ending} onClick={endSession}>
          {ending ? 'saving…' : 'end session'}
        </button>
      </div>
      <EditorContent editor={editor} />
    </>
  )
}
