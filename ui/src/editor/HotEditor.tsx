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
import { errText, notify } from '../Notice'
import { viewersWriteChip } from '../live'
import { makeExtensions, parser, schema } from './DocEditor'

/** How long a join may sit on "connecting…" before we give up and explain. */
const JOIN_GRACE_MS = 10_000

const CARET_COLORS = ['#8b9dc3', '#95c99b', '#d9b47a', '#d98a94', '#a88bd4', '#7bc4c4']
function colorFor(name: string): string {
  let h = 0
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0
  return CARET_COLORS[h % CARET_COLORS.length]
}

/** hot/status.agent — the room's agent, owner side. */
export interface AgentStatus {
  busy: boolean
  last_error?: string | null
  last_ok?: string | null
  asks?: number
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
  readOnly = false,
  canEnd = true,
  viewersWrite,
  onToggleViewersWrite,
  agent,
  onAsk,
}: {
  doc: HotDoc
  /** session over (we ended it, or the daemon dropped it) — reload cold */
  onEnded: () => void
  /** this participant may not write in the session (the owner set "watch
   * only", or an older daemon's view share): no typing, no ending — the
   * owner's daemon drops any writes anyway. Flips live mid-session. */
  readOnly?: boolean
  /** May this participant END the session? Owners and `propose` grantees
   * can; a `view` grantee riffing under session = consent cannot (the owner's
   * daemon refuses), so the button is hidden even while they can type. */
  canEnd?: boolean
  /** OWNED docs only: whether every share participant may edit this session
   * (session = consent) or just watch. Undefined hides the chip (mirror, or
   * a daemon that doesn't report it). */
  viewersWrite?: boolean
  onToggleViewersWrite?: (enabled: boolean) => void
  /** Agents in the room: present on owned docs. `agent` mirrors hot/status. */
  agent?: AgentStatus
  onAsk?: (instruction: string) => Promise<void>
}) {
  const [ask, setAsk] = useState('')
  const [asking, setAsking] = useState(false)
  const submitAsk = async () => {
    const q = ask.trim()
    if (!q || !onAsk) return
    setAsking(true)
    try {
      await onAsk(q)
      setAsk('')
    } finally {
      setAsking(false)
    }
  }
  const [peerNames, setPeerNames] = useState<string[]>([])
  const [me, setMe] = useState<string>('me')
  const [connected, setConnected] = useState(false)
  const [ending, setEnding] = useState(false)
  const endedRef = useRef(false)
  /** last bridge-failure reason we already told the user (avoid toast spam
   * while the provider retries) */
  const bridgeErrRef = useRef<string | null>(null)

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
      api<{ hot: boolean; bridge_error?: string }>(`/api/doc/${doc.docId}/hot/status`)
        .then((st) => {
          if (!st.hot && !endedRef.current) {
            endedRef.current = true
            onEnded()
            return
          }
          // the owner IS live but our daemon could not reach their session:
          // say why (once per distinct reason) instead of spinning silently
          if (st.hot && st.bridge_error && st.bridge_error !== bridgeErrRef.current) {
            bridgeErrRef.current = st.bridge_error
            notify(`can’t reach the owner’s live session — ${st.bridge_error}. Retrying…`)
          }
        })
        .catch((e) => {
          // Grimoire unreachable: say so rather than retrying silently forever
          if (endedRef.current) return
          endedRef.current = true
          notify(`lost the live session: ${errText(e)}`)
          onEnded()
        })
    }
    provider.on('connection-close', onClose)
    // never sit on "connecting…" forever: if the socket has not synced within
    // the grace period, ask the daemon why and fall back to the cold view
    let everConnected = false
    const onStatusOnce = ({ status }: { status: string }) => {
      if (status === 'connected') everConnected = true
    }
    provider.on('status', onStatusOnce)
    const joinTimer = setTimeout(() => {
      if (everConnected || endedRef.current) return
      api<{ hot: boolean }>(`/api/doc/${doc.docId}/hot/status`)
        .then((st) => {
          if (endedRef.current) return
          endedRef.current = true
          notify(
            st.hot
              ? 'could not join the live session (the owner’s Grimoire did not accept the connection) — showing the last saved version'
              : 'the live session ended before we could join — showing the last saved version',
          )
          onEnded()
        })
        .catch((e) => {
          if (endedRef.current) return
          endedRef.current = true
          notify(`could not join the live session: ${errText(e)}`)
          onEnded()
        })
    }, JOIN_GRACE_MS)
    return () => {
      clearTimeout(joinTimer)
      provider.off('status', onStatusOnce)
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
        // Yjs owns history in collab mode: StarterKit's nested undo/redo is
        // switched off at configure time (a name filter never matched it)
        ...makeExtensions({ history: false }),
        Collaboration.configure({ document: ydoc, field: 'default' }),
        CollaborationCaret.configure({
          provider,
          user: { name: me, color: colorFor(me) },
        }),
      ],
      editable: !readOnly,
      // content comes exclusively from the Yjs doc
    },
    // readOnly is deliberately NOT a dep: rebuilding the editor would re-bind
    // the Yjs binding and drop the caret; setEditable below flips it live
    [doc.docId, me],
  )

  // a mid-session flip of can_write (owner toggled "watch only") must take
  // effect at once, without a remount
  useEffect(() => {
    if (!editor || editor.isDestroyed) return
    if (editor.isEditable === !readOnly) return
    editor.setEditable(!readOnly)
  }, [editor, readOnly])

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
      // the daemon session is still open: stay live so the button can be
      // retried instead of exiting the UI while the doc is still hot
      notify(String(e))
      endedRef.current = false
      setEnding(false)
      return
    }
    onEnded()
  }

  return (
    <>
      <div className={`hot-banner ${readOnly ? 'readonly' : ''}`}>
        <span className="hot-dot" />
        {readOnly ? '👁 watching live · read-only' : 'live'}
        {peerNames.length > 0
          ? ` with ${peerNames.join(', ')}`
          : readOnly
            ? ''
            : ' — waiting for others'}
        {' · '}
        {connected ? 'synced' : 'connecting…'}
        {typeof viewersWrite === 'boolean' && onToggleViewersWrite && (
          <button
            className={`chip viewers-write ${viewersWrite ? 'on' : ''}`}
            title={viewersWriteChip(viewersWrite).title}
            onClick={() => onToggleViewersWrite(!viewersWrite)}
          >
            {viewersWriteChip(viewersWrite).label}
          </button>
        )}
        {!readOnly && canEnd && (
          <button className="hot-end" disabled={ending} onClick={endSession}>
            {ending ? 'saving…' : 'end session'}
          </button>
        )}
      </div>
      {onAsk && !readOnly && (
        <div className="room-ask">
          <span className="room-ask-mark" title="the room's agent — everything it writes is a suggestion you accept or reject">🌿</span>
          <input
            className="room-ask-input"
            placeholder={agent?.busy ? 'scribe is thinking…' : 'ask the room\u2019s agent — e.g. “tighten the intro”, “pull the decision out of this thread”'}
            value={ask}
            disabled={asking || !!agent?.busy}
            onChange={(e) => setAsk(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault()
                submitAsk()
              }
            }}
          />
          {agent?.busy ? (
            <span className="room-ask-status busy">thinking…</span>
          ) : agent?.last_error ? (
            <span className="room-ask-status error" title={agent.last_error}>
              didn’t work — {agent.last_error.length > 60 ? agent.last_error.slice(0, 60) + '…' : agent.last_error}
            </span>
          ) : agent?.last_ok ? (
            <span className="room-ask-status ok">{agent.last_ok}</span>
          ) : null}
        </div>
      )}
      <EditorContent editor={editor} />
    </>
  )
}
