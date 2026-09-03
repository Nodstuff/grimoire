// Canvas block v2 (#68): a React Flow diagram editor whose ks_diagram JSON is
// the block's content — one format for humans AND agents (the primer already
// teaches gardeners to write it). Lucid-grade behavior: shapes from a palette,
// connectors anchored to handles that never detach, snap grid, tidy-up layout.
// Live co-drawing (P2.5) rides the hot-session transport: shape-level LWW.
// The pure format/conversion logic lives in ./canvas/convert.ts.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  Background,
  BackgroundVariant,
  Controls,
  Handle,
  MiniMap,
  NodeResizer,
  Position,
  ReactFlow,
  ReactFlowProvider,
  addEdge,
  useEdgesState,
  useNodesState,
  useReactFlow,
  type Connection,
  type Edge,
  type Node,
  type NodeProps,
} from '@xyflow/react'
import {
  BaseEdge,
  EdgeLabelRenderer,
  getNodesBounds,
  getSmoothStepPath,
  getViewportForBounds,
  type EdgeProps,
} from '@xyflow/react'
import { toPng, toSvg } from 'html-to-image'
import * as Y from 'yjs'
import { WebsocketProvider } from 'y-websocket'
import '@xyflow/react/dist/style.css'
import { api, Block } from './types'
import { debounce, throttle } from './timing'
import { saveErrorText } from './hints'
import { notify } from './Notice'

import {
  COLORS,
  DEFAULT_SIZE,
  decorate,
  diffShapes,
  fromFlow,
  layout,
  mergeShapes,
  parseContent,
  serializeShapes,
  toFlow,
  type CanvasEdgeData,
  type CanvasNodeData,
  type ShapeKind,
} from './canvas/convert'

/** Real geometry in a 0..100 space, stroked crisply at any aspect ratio via
 * vector-effect. The CSS-rotation diamond of v2.0 looked like an off-kilter
 * rectangle — never again. */
const SVG_SHAPES: Partial<Record<ShapeKind, string>> = {
  diamond: 'M 50 1 L 99 50 L 50 99 L 1 50 Z',
  parallelogram: 'M 18 1 L 99 1 L 82 99 L 1 99 Z',
  hexagon: 'M 22 1 L 78 1 L 99 50 L 78 99 L 22 99 L 1 50 Z',
  cylinder: 'M 1 14 A 49 13 0 0 1 99 14 L 99 86 A 49 13 0 0 1 1 86 Z M 1 14 A 49 13 0 0 0 99 14',
  document: 'M 1 1 L 99 1 L 99 82 C 75 70 60 98 38 90 C 22 84 10 92 1 84 Z',
  cloud:
    'M 25 90 C 8 90 1 78 1 66 C 1 54 10 46 20 46 C 22 30 34 20 48 22 C 58 8 82 10 88 28 C 97 32 99 44 97 54 C 104 68 94 90 78 90 Z',
  actor:
    'M 50 1 A 13 13 0 1 1 49.9 1 M 50 27 L 50 62 M 50 36 L 18 30 M 50 36 L 82 30 M 50 62 L 24 99 M 50 62 L 76 99',
  triangle: 'M 50 1 L 99 99 L 1 99 Z',
}
/** Shapes whose path is line-art, not a fillable outline. */
const OPEN_SHAPES = new Set<ShapeKind>(['actor'])

/* ---------- custom nodes ---------- */

function wikiSplit(label: string): (string | { link: string })[] {
  const parts: (string | { link: string })[] = []
  let rest = label
  for (;;) {
    const i = rest.indexOf('[[')
    if (i < 0) break
    const j = rest.indexOf(']]', i)
    if (j < 0) break
    if (i > 0) parts.push(rest.slice(0, i))
    parts.push({ link: rest.slice(i + 2, j).split(/[|#]/)[0].trim() })
    rest = rest.slice(j + 2)
  }
  if (rest) parts.push(rest)
  return parts
}

function ShapeNode({ id, data, selected }: NodeProps<Node<CanvasNodeData>>) {
  const { setNodes } = useReactFlow()
  const [editing, setEditing] = useState(false)
  const kind = data.kind

  const commit = (label: string) => {
    setEditing(false)
    setNodes((ns) => ns.map((n) => (n.id === id ? { ...n, data: { ...n.data, label } } : n)))
  }

  const handles = (
    <>
      <Handle id="t" type="source" position={Position.Top} />
      <Handle id="b" type="source" position={Position.Bottom} />
      <Handle id="l" type="source" position={Position.Left} />
      <Handle id="r" type="source" position={Position.Right} />
    </>
  )

  const label = editing ? (
    <textarea
      className="cnode-edit nodrag"
      autoFocus
      defaultValue={data.label}
      onBlur={(e) => commit(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === 'Enter' && !e.shiftKey) {
          e.preventDefault()
          commit((e.target as HTMLTextAreaElement).value)
        }
        if (e.key === 'Escape') setEditing(false)
      }}
    />
  ) : (
    <span className="cnode-label" onDoubleClick={() => setEditing(true)}>
      {wikiSplit(data.label).map((p, i) =>
        typeof p === 'string' ? (
          <span key={i}>{p}</span>
        ) : (
          <a
            key={i}
            className="cnode-link nodrag"
            onClick={(e) => {
              e.stopPropagation()
              window.dispatchEvent(new CustomEvent('grimoire:open-doc', { detail: p.link }))
            }}
          >
            {p.link}
          </a>
        ),
      )}
      {!data.label && <span className="cnode-hint">double-click</span>}
    </span>
  )

  if (kind === 'frame') {
    return (
      <div className="cnode cnode-frame" style={{ borderColor: data.color }}>
        <NodeResizer isVisible={!!selected} minWidth={160} minHeight={120} />
        <div className="cnode-frame-title">{label}</div>
      </div>
    )
  }
  const svgPath = SVG_SHAPES[kind]
  return (
    <div
      className={`cnode cnode-${kind} ${selected ? 'sel' : ''} ${svgPath ? 'cnode-svg' : ''}`}
      style={
        kind === 'sticky'
          ? { background: data.color, color: '#101014' }
          : svgPath
            ? undefined
            : { borderColor: data.color }
      }
    >
      <NodeResizer isVisible={!!selected} minWidth={60} minHeight={36} />
      {svgPath && (
        <svg
          className="cnode-shape"
          viewBox="0 0 100 100"
          preserveAspectRatio="none"
          aria-hidden
        >
          <path
            d={svgPath}
            fill={OPEN_SHAPES.has(kind) ? 'none' : 'var(--panel)'}
            stroke={data.color}
            strokeWidth={1.5}
            strokeLinecap="round"
            vectorEffect="non-scaling-stroke"
          />
        </svg>
      )}
      {label}
      {handles}
    </div>
  )
}

const nodeTypes = { shape: ShapeNode }

/** Connector with a double-click-editable label, Lucid-style. */
function LabelEdge(props: EdgeProps) {
  const { setEdges } = useReactFlow()
  const [editing, setEditing] = useState(false)
  const [path, labelX, labelY] = getSmoothStepPath(props)
  const label = typeof props.label === 'string' ? props.label : ''

  const commit = (v: string) => {
    setEditing(false)
    setEdges((es) => es.map((e) => (e.id === props.id ? { ...e, label: v || undefined } : e)))
  }

  return (
    <>
      <BaseEdge
        id={props.id}
        path={path}
        markerEnd={props.markerEnd}
        markerStart={props.markerStart}
        style={props.style}
      />
      {/* generous invisible hit area for the double-click */}
      <path
        d={path}
        fill="none"
        strokeWidth={16}
        stroke="transparent"
        onDoubleClick={() => setEditing(true)}
      />
      <EdgeLabelRenderer>
        <div
          className={`cedge-label ${props.selected ? 'sel' : ''}`}
          style={{ transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)` }}
        >
          {editing ? (
            <input
              className="cedge-label-edit nodrag nopan"
              autoFocus
              defaultValue={label}
              onBlur={(e) => commit(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') commit((e.target as HTMLInputElement).value)
                if (e.key === 'Escape') setEditing(false)
              }}
            />
          ) : (
            <span
              className={label ? 'cedge-label-text nopan' : 'cedge-label-empty nopan'}
              onDoubleClick={() => setEditing(true)}
            >
              {label}
            </span>
          )}
        </div>
      </EdgeLabelRenderer>
    </>
  )
}

const edgeTypes = { label: LabelEdge }

/* ---------- the editor ---------- */

function CanvasFlow({
  block,
  epoch,
  onSaved,
}: {
  block: Block
  epoch: number
  onSaved: () => void
}) {
  const initial = useMemo(() => toFlow(parseContent(block.content)), [block.id])
  const [nodes, setNodes, onNodesChange] = useNodesState(initial.nodes)
  const [edges, setEdges, onEdgesChange] = useEdgesState(initial.edges)
  const { screenToFlowPosition, fitView } = useReactFlow()
  const epochRef = useRef(epoch)
  const dirty = useRef(false)
  const fitTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => {
    epochRef.current = epoch
  }, [epoch])

  /* ---- live canvas (P2.5): shape-level LWW over the hot-session ws ----
   * Shapes live in Y.Maps (canvas_nodes / canvas_edges, values = JSON
   * strings): concurrent editors merge per shape; the daemon flattens the
   * maps back to ks_diagram at session end. */
  const [live, setLive] = useState<{ seed: boolean } | null>(null)
  const liveRef = useRef(live)
  liveRef.current = live
  const yRef = useRef<{
    ydoc: Y.Doc
    provider: WebsocketProvider
    nodesMap: Y.Map<string>
    edgesMap: Y.Map<string>
  } | null>(null)
  const lastPushed = useRef<{ nodes: Map<string, string>; edges: Map<string, string> }>({
    nodes: new Map(),
    edges: new Map(),
  })
  const [peerCount, setPeerCount] = useState(1)

  const goLiveCanvas = async () => {
    try {
      const r = await api<{ seed: boolean }>(`/api/doc/${block.doc_id}/hot/start`, {
        method: 'POST',
      })
      setLive({ seed: r.seed })
    } catch (e) {
      notify(String(e))
    }
  }
  const endLiveCanvas = async () => {
    try {
      await api(`/api/doc/${block.doc_id}/hot/end`, { method: 'POST' })
    } catch (e) {
      notify(String(e)) // session still open on the daemon: stay live
      return
    }
    setLive(null)
    onSaved()
  }

  // auto-join a session someone else started
  useEffect(() => {
    if (live) return
    const t = setInterval(() => {
      api<{ hot: boolean }>(`/api/doc/${block.doc_id}/hot/status`)
        .then((st) => {
          if (st.hot && !liveRef.current) setLive({ seed: false })
        })
        .catch((e) => console.warn('canvas live check failed', block.doc_id, e))
    }, 5000)
    return () => clearInterval(t)
  }, [live, block.doc_id])

  useEffect(() => {
    if (!live) return
    const ydoc = new Y.Doc()
    const nodesMap = ydoc.getMap<string>('canvas_nodes')
    const edgesMap = ydoc.getMap<string>('canvas_edges')
    const proto = location.protocol === 'https:' ? 'wss' : 'ws'
    const provider = new WebsocketProvider(`${proto}://${location.host}/ws/hot`, block.doc_id, ydoc)
    yRef.current = { ydoc, provider, nodesMap, edgesMap }

    const serialize = () => serializeShapes(fromFlow(nodesRef.current, edgesRef.current))

    provider.on('sync', (isSynced: boolean) => {
      if (isSynced && live.seed && nodesMap.size === 0) {
        const cur = serialize()
        ydoc.transact(() => {
          cur.nodes.forEach((v, k) => nodesMap.set(k, v))
          cur.edges.forEach((v, k) => edgesMap.set(k, v))
        }, 'local')
        lastPushed.current = cur
      }
    })

    const applyRemote = () => {
      const merged = mergeShapes(nodesMap.entries(), edgesMap.entries())
      lastPushed.current = merged.pushed
      const flow = toFlow(merged.diagram)
      const selected = new Set(nodesRef.current.filter((n) => n.selected).map((n) => n.id))
      const selectedE = new Set(edgesRef.current.filter((e) => e.selected).map((e) => e.id))
      restoring.current = true // remote apply is not an undo step of ours
      setNodes(flow.nodes.map((n) => ({ ...n, selected: selected.has(n.id) })))
      setEdges(flow.edges.map((e) => ({ ...e, selected: selectedE.has(e.id) })))
    }
    const onMaps = (_e: unknown, txn: Y.Transaction) => {
      if (txn.origin === 'local') return
      applyRemote()
    }
    nodesMap.observe(onMaps)
    edgesMap.observe(onMaps)

    const onAwareness = () => setPeerCount(provider.awareness.getStates().size)
    provider.awareness.on('change', onAwareness)
    provider.awareness.setLocalStateField('user', { kind: 'canvas' })

    // session ended elsewhere: fall back to cold and reload
    const onClose = () => {
      api<{ hot: boolean }>(`/api/doc/${block.doc_id}/hot/status`)
        .then((st) => {
          if (!st.hot && liveRef.current) {
            setLive(null)
            onSaved()
          }
        })
        .catch((e) => console.warn('canvas session status failed', block.doc_id, e))
    }
    provider.on('connection-close', onClose)

    return () => {
      liveThrottle.current.flush()
      provider.awareness.off('change', onAwareness)
      provider.off('connection-close', onClose)
      provider.destroy()
      ydoc.destroy()
      yRef.current = null
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [live, block.doc_id])

  // local structural changes while live: push per-shape LWW updates. A drag
  // emits a change per frame; the throttle collapses that to ~16 pushes/s
  // with a trailing call so the final position always lands.
  const pushLiveNow = useCallback(() => {
    const y = yRef.current
    if (!y) return
    const now = serializeShapes(fromFlow(nodesRef.current, edgesRef.current))
    const delta = diffShapes(lastPushed.current, now)
    y.ydoc.transact(() => {
      for (const [k, v] of delta.nodes.set) y.nodesMap.set(k, v)
      for (const k of delta.nodes.del) y.nodesMap.delete(k)
      for (const [k, v] of delta.edges.set) y.edgesMap.set(k, v)
      for (const k of delta.edges.del) y.edgesMap.delete(k)
    }, 'local')
    lastPushed.current = now
  }, [])
  const pushLiveRef = useRef(pushLiveNow)
  pushLiveRef.current = pushLiveNow
  const liveThrottle = useRef(throttle(60, () => pushLiveRef.current()))
  const pushLive = useCallback(() => liveThrottle.current.call(), [])

  // save through the gate, same as everything else. Mirrors DocEditor: a
  // failure keeps the canvas dirty, says so ONCE per distinct cause, and a
  // slow clock retries until it lands — a swallowed failure here was how
  // canvas edits used to vanish.
  const saving = useRef(false)
  const lastSaveError = useRef<string | null>(null)
  const saveNow = useCallback(async () => {
    if (!dirty.current || saving.current || liveRef.current) return
    const snapshot = { nodes: nodesRef.current, edges: edgesRef.current }
    const d = fromFlow(
      (snapshot.nodes ?? []) as Node<CanvasNodeData>[],
      snapshot.edges ?? [],
    )
    saving.current = true
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
                content: JSON.stringify({ ks_diagram: d }),
              },
              source_refs: ['canvas:edit'],
            },
          ],
        }),
      })
      epochRef.current = out.epoch
      lastSaveError.current = null
      // edits during the request are not in what landed: stay dirty
      const editedMeanwhile =
        nodesRef.current !== snapshot.nodes || edgesRef.current !== snapshot.edges
      dirty.current = editedMeanwhile
      if (editedMeanwhile) autosave.current.arm()
      onSaved()
    } catch (e) {
      const msg = saveErrorText(e)
      if (lastSaveError.current !== msg) {
        lastSaveError.current = msg
        notify(msg, 'warn')
      }
      console.error('canvas save failed', e)
    } finally {
      saving.current = false
    }
  }, [block.id, block.doc_id, onSaved])
  const saveRef = useRef(saveNow)
  saveRef.current = saveNow
  const autosave = useRef(debounce(1200, () => saveRef.current()))
  const scheduleSave = useCallback(() => {
    dirty.current = true
    autosave.current.arm()
  }, [])
  // retry clock while dirty (daemon back, live session over, …)
  useEffect(() => {
    const t = setInterval(() => {
      if (dirty.current && !autosave.current.pending()) saveRef.current()
    }, 5000)
    return () => clearInterval(t)
  }, [])
  // unmount/navigation: land whatever is pending, drop the undo coalescer
  useEffect(() => {
    const pending = autosave.current
    return () => {
      pending.cancel()
      if (snapTimer.current) clearTimeout(snapTimer.current)
      if (fitTimer.current) clearTimeout(fitTimer.current)
      if (dirty.current) saveRef.current()
    }
  }, [])

  const nodesRef = useRef(nodes)
  nodesRef.current = nodes
  const edgesRef = useRef(edges)
  edgesRef.current = edges

  // undo/redo: checkpoint snapshots, coalescing bursts (drags emit dozens of
  // changes — one checkpoint per ~400ms quiet gap is what a human calls a step)
  type Snap = { nodes: Node<CanvasNodeData>[]; edges: Edge[] }
  const past = useRef<Snap[]>([])
  const future = useRef<Snap[]>([])
  const lastSnap = useRef<Snap>({ nodes: initial.nodes, edges: initial.edges })
  const snapTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const restoring = useRef(false)

  // any structural change: checkpoint for undo + persist (live sessions
  // stream per-shape updates instead of proposing — the epoch is frozen)
  useEffect(() => {
    if (nodes === initial.nodes && edges === initial.edges) return
    if (restoring.current) {
      restoring.current = false
      lastSnap.current = { nodes, edges }
      if (liveRef.current) pushLive()
      else scheduleSave()
      return
    }
    if (snapTimer.current) clearTimeout(snapTimer.current)
    snapTimer.current = setTimeout(() => {
      snapTimer.current = null
      past.current.push(lastSnap.current)
      if (past.current.length > 100) past.current.shift()
      future.current = []
      lastSnap.current = { nodes: nodesRef.current, edges: edgesRef.current }
    }, 400)
    if (liveRef.current) pushLive()
    else scheduleSave()
  }, [nodes, edges, initial, scheduleSave, pushLive])

  const undo = useCallback(() => {
    // a pending checkpoint means changes newer than lastSnap: flush it first
    if (snapTimer.current) {
      clearTimeout(snapTimer.current)
      snapTimer.current = null
      past.current.push(lastSnap.current)
      lastSnap.current = { nodes: nodesRef.current, edges: edgesRef.current }
    }
    const prev = past.current.pop()
    if (!prev) return
    future.current.push({ nodes: nodesRef.current, edges: edgesRef.current })
    restoring.current = true
    lastSnap.current = prev
    setNodes(prev.nodes)
    setEdges(prev.edges)
  }, [setNodes, setEdges])

  const redo = useCallback(() => {
    const next = future.current.pop()
    if (!next) return
    past.current.push({ nodes: nodesRef.current, edges: edgesRef.current })
    restoring.current = true
    lastSnap.current = next
    setNodes(next.nodes)
    setEdges(next.edges)
  }, [setNodes, setEdges])

  // clipboard: selected nodes + the edges fully inside the selection
  const clipboard = useRef<Snap | null>(null)
  const copySelection = useCallback(() => {
    const ns = nodesRef.current.filter((n) => n.selected)
    if (ns.length === 0) return
    const ids = new Set(ns.map((n) => n.id))
    const es = edgesRef.current.filter((e) => ids.has(e.source) && ids.has(e.target))
    clipboard.current = { nodes: ns, edges: es }
  }, [])
  const pasteClipboard = useCallback(
    (offset = 28) => {
      const clip = clipboard.current
      if (!clip) return
      const idMap = new Map(clip.nodes.map((n) => [n.id, crypto.randomUUID()]))
      const newNodes = clip.nodes.map((n) => ({
        ...n,
        id: idMap.get(n.id)!,
        position: { x: n.position.x + offset, y: n.position.y + offset },
        selected: true,
      }))
      const newEdges = clip.edges.map((e) => ({
        ...e,
        id: crypto.randomUUID(),
        source: idMap.get(e.source)!,
        target: idMap.get(e.target)!,
        selected: false,
      }))
      setNodes((ns) => [...ns.map((n) => ({ ...n, selected: false })), ...newNodes])
      setEdges((es) => [...es, ...newEdges])
    },
    [setNodes, setEdges],
  )
  const duplicate = useCallback(() => {
    copySelection()
    pasteClipboard()
  }, [copySelection, pasteClipboard])

  const onCanvasKey = useCallback(
    (e: React.KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey
      if (!mod) return
      const inField = /INPUT|TEXTAREA/.test((e.target as HTMLElement).tagName)
      if (inField) return
      const k = e.key.toLowerCase()
      if (k === 'z') {
        e.preventDefault()
        e.stopPropagation()
        if (e.shiftKey) redo()
        else undo()
      } else if (k === 'c') {
        e.preventDefault()
        copySelection()
      } else if (k === 'v') {
        e.preventDefault()
        pasteClipboard()
      } else if (k === 'd') {
        e.preventDefault()
        e.stopPropagation()
        duplicate()
      } else if (k === 'a') {
        e.preventDefault()
        e.stopPropagation()
        setNodes((ns) => ns.map((n) => ({ ...n, selected: true })))
      }
    },
    [undo, redo, copySelection, pasteClipboard, duplicate, setNodes],
  )

  // edge styling: operate on the selected edges
  const restyleEdges = useCallback(
    (f: (d: CanvasEdgeData) => CanvasEdgeData) => {
      setEdges((es) =>
        es.map((e) =>
          e.selected ? decorate({ ...e, data: f((e.data ?? {}) as CanvasEdgeData) }) : e,
        ),
      )
    },
    [setEdges],
  )
  const cycleArrow = () =>
    restyleEdges((d) => ({
      ...d,
      arrow: d.arrow === 'end' ? 'both' : d.arrow === 'both' ? 'none' : 'end',
    }))
  const toggleDash = () => restyleEdges((d) => ({ ...d, dash: !d.dash }))
  const anyEdgeSelected = edges.some((e) => e.selected)
  const selectedNodes = nodes.filter((n) => n.selected && n.data.kind !== 'frame')

  // align/distribute the selection
  const align = (axis: 'h' | 'v') => {
    const sel = selectedNodes
    if (sel.length < 2) return
    if (axis === 'h') {
      const cy =
        sel.reduce((a, n) => a + n.position.y + (n.height ?? 64) / 2, 0) / sel.length
      setNodes((ns) =>
        ns.map((n) =>
          n.selected && n.data.kind !== 'frame'
            ? { ...n, position: { ...n.position, y: cy - (n.height ?? 64) / 2 } }
            : n,
        ),
      )
    } else {
      const cx =
        sel.reduce((a, n) => a + n.position.x + (n.width ?? 180) / 2, 0) / sel.length
      setNodes((ns) =>
        ns.map((n) =>
          n.selected && n.data.kind !== 'frame'
            ? { ...n, position: { ...n.position, x: cx - (n.width ?? 180) / 2 } }
            : n,
        ),
      )
    }
  }
  const distribute = (axis: 'h' | 'v') => {
    const sel = [...selectedNodes]
    if (sel.length < 3) return
    const key = axis === 'h' ? 'x' : 'y'
    sel.sort((a, b) => a.position[key] - b.position[key])
    const first = sel[0].position[key]
    const last = sel[sel.length - 1].position[key]
    const step = (last - first) / (sel.length - 1)
    const target = new Map(sel.map((n, i) => [n.id, first + i * step]))
    setNodes((ns) =>
      ns.map((n) =>
        target.has(n.id)
          ? { ...n, position: { ...n.position, [key]: target.get(n.id)! } }
          : n,
      ),
    )
  }

  // export: render the flow viewport to an image, save via the daemon
  const exportAs = async (format: 'png' | 'svg') => {
    const el = document.querySelector('.react-flow__viewport') as HTMLElement | null
    if (!el) return
    const bounds = getNodesBounds(nodesRef.current)
    const W = Math.min(4096, Math.max(640, Math.ceil(bounds.width + 120)))
    const H = Math.min(4096, Math.max(480, Math.ceil(bounds.height + 120)))
    const vp = getViewportForBounds(bounds, W, H, 0.2, 2, 0.06)
    const opts = {
      backgroundColor: '#101014',
      width: W,
      height: H,
      style: {
        width: `${W}px`,
        height: `${H}px`,
        transform: `translate(${vp.x}px, ${vp.y}px) scale(${vp.zoom})`,
      },
    }
    try {
      const dataUrl =
        format === 'png' ? await toPng(el, opts) : await toSvg(el, opts)
      const r = await api<{ path: string }>('/api/export', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          filename: `canvas-${new Date().toISOString().slice(0, 10)}.${format}`,
          data_url: dataUrl,
        }),
      })
      notify(`saved to ${r.path}`, 'ok')
    } catch (e) {
      notify(String(e))
    }
  }

  const onConnect = useCallback(
    (c: Connection) =>
      setEdges((es) =>
        addEdge(
          decorate({
            ...c,
            id: crypto.randomUUID(),
            type: 'label',
            data: { arrow: 'end' },
          } as Edge),
          es,
        ),
      ),
    [setEdges],
  )

  const addNode = (kind: ShapeKind) => {
    const [w, h] = DEFAULT_SIZE[kind]
    const p = screenToFlowPosition({
      x: window.innerWidth / 2,
      y: window.innerHeight / 2,
    })
    setNodes((ns) => [
      ...ns,
      {
        id: crypto.randomUUID(),
        type: 'shape',
        position: { x: p.x - w / 2, y: p.y - h / 2 },
        width: w,
        height: h,
        zIndex: kind === 'frame' ? -1 : 0,
        data: { label: '', color: COLORS[(ns.length + (kind === 'sticky' ? 2 : 0)) % 6], kind },
      },
    ])
  }

  const recolor = () => {
    setNodes((ns) =>
      ns.map((n) =>
        n.selected
          ? {
              ...n,
              data: {
                ...n.data,
                color: COLORS[(COLORS.indexOf(n.data.color) + 1) % COLORS.length],
              },
            }
          : n,
      ),
    )
    setEdges((es) =>
      es.map((e) => {
        if (!e.selected) return e
        const d = (e.data ?? {}) as CanvasEdgeData
        const color = COLORS[(COLORS.indexOf(d.color ?? '') + 1) % COLORS.length]
        return decorate({ ...e, data: { ...d, color } })
      }),
    )
  }

  const tidy = () => {
    const d = fromFlow(nodes, edges)
    const pos = layout({ nodes: d.nodes.map((n) => ({ ...n, x: undefined, y: undefined })), edges: d.edges })
    setNodes((ns) =>
      ns.map((n) =>
        n.data.kind === 'frame' ? n : { ...n, position: pos.get(n.id) ?? n.position },
      ),
    )
    if (fitTimer.current) clearTimeout(fitTimer.current)
    fitTimer.current = setTimeout(() => fitView({ padding: 0.15 }), 50)
  }

  return (
    <div
      className="canvas-wrap"
      tabIndex={0}
      onKeyDownCapture={onCanvasKey}
    >
      <div className="canvas-toolbar">
        <button onClick={() => addNode('box')} title="process">▭</button>
        <button onClick={() => addNode('pill')} title="start / end">⬭</button>
        <button onClick={() => addNode('diamond')} title="decision">◇</button>
        <button onClick={() => addNode('parallelogram')} title="input / output">▱</button>
        <button onClick={() => addNode('hexagon')} title="preparation">⬡</button>
        <button onClick={() => addNode('cylinder')} title="datastore">⛁</button>
        <button onClick={() => addNode('ellipse')} title="ellipse">◯</button>
        <button onClick={() => addNode('sticky')} title="sticky note">🗒</button>
        <button onClick={() => addNode('text')} title="text label">T</button>
        <button onClick={() => addNode('frame')} title="frame">⬚</button>
        <span className="canvas-toolbar-sep" />
        <button onClick={() => addNode('document')} title="document">🗎</button>
        <button onClick={() => addNode('cloud')} title="cloud">☁</button>
        <button onClick={() => addNode('actor')} title="actor">🧍</button>
        <button onClick={() => addNode('triangle')} title="triangle">△</button>
        <span className="canvas-toolbar-sep" />
        <button onClick={recolor} title="cycle color of selection">🎨</button>
        <button onClick={tidy} title="auto-layout">✨</button>
        {anyEdgeSelected && (
          <>
            <span className="canvas-toolbar-sep" />
            <button onClick={cycleArrow} title="arrowheads: end → both → none">➔</button>
            <button onClick={toggleDash} title="toggle dashed">┄</button>
          </>
        )}
        {selectedNodes.length >= 2 && (
          <>
            <span className="canvas-toolbar-sep" />
            <button onClick={() => align('h')} title="align horizontally">⭤</button>
            <button onClick={() => align('v')} title="align vertically">⭥</button>
            {selectedNodes.length >= 3 && (
              <>
                <button onClick={() => distribute('h')} title="distribute horizontally">⋯</button>
                <button onClick={() => distribute('v')} title="distribute vertically">⋮</button>
              </>
            )}
          </>
        )}
        <span className="canvas-toolbar-sep" />
        <button onClick={() => exportAs('png')} title="export PNG">🖼</button>
        <button onClick={() => exportAs('svg')} title="export SVG">⬇</button>
        <span className="canvas-toolbar-sep" />
        {live ? (
          <button className="canvas-live-btn on" onClick={endLiveCanvas} title="end live session">
            ⏹ live · {peerCount}
          </button>
        ) : (
          <button className="canvas-live-btn" onClick={goLiveCanvas} title="start live co-drawing">
            ⚡
          </button>
        )}
      </div>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={nodeTypes}
        edgeTypes={edgeTypes}
        onNodesChange={onNodesChange}
        onEdgesChange={onEdgesChange}
        onConnect={onConnect}
        connectionMode={'loose' as never}
        fitView
        snapToGrid
        snapGrid={[10, 10]}
        proOptions={{ hideAttribution: true }}
        colorMode="dark"
        defaultEdgeOptions={{ type: 'label' }}
      >
        <Background variant={BackgroundVariant.Dots} gap={20} size={1} />
        <Controls showInteractive={false} />
        <MiniMap pannable zoomable />
      </ReactFlow>
    </div>
  )
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
  return (
    <div className={full ? 'canvas-full' : 'canvas-block'}>
      <ReactFlowProvider>
        <CanvasFlow block={block} epoch={epoch} onSaved={onSaved} />
      </ReactFlowProvider>
    </div>
  )
}
