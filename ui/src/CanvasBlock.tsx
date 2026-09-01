// Canvas block v2 (#68): a React Flow diagram editor whose ks_diagram JSON is
// the block's content — one format for humans AND agents (the primer already
// teaches gardeners to write it). Lucid-grade behavior: shapes from a palette,
// connectors anchored to handles that never detach, snap grid, tidy-up layout.
// Old tldraw snapshots convert best-effort on load; the first save persists
// ks_diagram and the tldraw era is over. No live sync yet (P2.5 rides the
// hot-session transport when it comes).

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
  getSmoothStepPath,
  type EdgeProps,
} from '@xyflow/react'
import '@xyflow/react/dist/style.css'
import { api, Block } from './types'

/* ---------- storage format ---------- */

type ShapeKind =
  | 'box'
  | 'pill'
  | 'ellipse'
  | 'diamond'
  | 'parallelogram'
  | 'hexagon'
  | 'cylinder'
  | 'sticky'
  | 'text'
  | 'frame'

interface KsNode {
  id: string
  label: string
  x?: number
  y?: number
  w?: number
  h?: number
  shape?: ShapeKind
  color?: string
}

interface KsEdge {
  id?: string
  from: string
  to: string
  label?: string
  fromSide?: string
  toSide?: string
}

interface KsDiagram {
  nodes: KsNode[]
  edges: KsEdge[]
}

const COLORS = ['#8b9dc3', '#95c99b', '#d9b47a', '#d98a94', '#a88bd4', '#7bc4c4', '#6b6b7b']
const DEFAULT_SIZE: Record<ShapeKind, [number, number]> = {
  box: [180, 64],
  pill: [170, 56],
  ellipse: [160, 90],
  diamond: [170, 110],
  parallelogram: [190, 70],
  hexagon: [180, 80],
  cylinder: [150, 100],
  sticky: [160, 140],
  text: [160, 40],
  frame: [420, 300],
}

/** Real geometry in a 0..100 space, stroked crisply at any aspect ratio via
 * vector-effect. The CSS-rotation diamond of v2.0 looked like an off-kilter
 * rectangle — never again. */
const SVG_SHAPES: Partial<Record<ShapeKind, string>> = {
  diamond: 'M 50 1 L 99 50 L 50 99 L 1 50 Z',
  parallelogram: 'M 18 1 L 99 1 L 82 99 L 1 99 Z',
  hexagon: 'M 22 1 L 78 1 L 99 50 L 78 99 L 22 99 L 1 50 Z',
  cylinder: 'M 1 14 A 49 13 0 0 1 99 14 L 99 86 A 49 13 0 0 1 1 86 Z M 1 14 A 49 13 0 0 0 99 14',
}

/* ---------- layered fallback layout (agent diagrams ship without x/y) ---------- */

function layout(d: KsDiagram): Map<string, { x: number; y: number }> {
  const pos = new Map<string, { x: number; y: number }>()
  const indeg = new Map<string, number>()
  for (const n of d.nodes) indeg.set(n.id, 0)
  for (const e of d.edges) indeg.set(e.to, (indeg.get(e.to) ?? 0) + 1)
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
    pos.set(n.id, { x: col * 300, y: row * 130 })
  }
  return pos
}

/* ---------- tldraw snapshot → ks_diagram (one-way, best effort) ---------- */

function convertTldraw(snapshot: Record<string, unknown>): KsDiagram {
  const store = (snapshot.document as { store?: Record<string, unknown> })?.store ?? {}
  const nodes: KsNode[] = []
  const edges: KsEdge[] = []
  const rich = (rt: unknown): string => {
    // tldraw richText: walk for text leaves
    const out: string[] = []
    const walk = (v: unknown) => {
      if (!v || typeof v !== 'object') return
      const o = v as Record<string, unknown>
      if (typeof o.text === 'string') out.push(o.text)
      if (Array.isArray(o.content)) o.content.forEach(walk)
    }
    walk(rt)
    return out.join(' ')
  }
  for (const rec of Object.values(store)) {
    const r = rec as Record<string, unknown>
    if (r.typeName !== 'shape') continue
    const props = (r.props ?? {}) as Record<string, unknown>
    if (r.type === 'geo' || r.type === 'text' || r.type === 'note') {
      nodes.push({
        id: String(r.id),
        label: rich(props.richText) || String(props.text ?? ''),
        x: Number(r.x ?? 0),
        y: Number(r.y ?? 0),
        w: Number(props.w ?? 180) || 180,
        h: Number(props.h ?? 64) || 64,
        shape: r.type === 'note' ? 'sticky' : props.geo === 'diamond' ? 'diamond' : 'box',
      })
    }
  }
  // arrows: match endpoints to nearest node centers
  const center = (n: KsNode) => ({ x: (n.x ?? 0) + (n.w ?? 180) / 2, y: (n.y ?? 0) + (n.h ?? 64) / 2 })
  const nearest = (p: { x: number; y: number }): KsNode | null => {
    let best: KsNode | null = null
    let bd = 200 * 200
    for (const n of nodes) {
      const c = center(n)
      const d = (c.x - p.x) ** 2 + (c.y - p.y) ** 2
      if (d < bd) {
        bd = d
        best = n
      }
    }
    return best
  }
  for (const rec of Object.values(store)) {
    const r = rec as Record<string, unknown>
    if (r.typeName !== 'shape' || r.type !== 'arrow') continue
    const props = (r.props ?? {}) as Record<string, unknown>
    const s = props.start as { x?: number; y?: number } | undefined
    const e = props.end as { x?: number; y?: number } | undefined
    if (!s || !e) continue
    const ax = Number(r.x ?? 0)
    const ay = Number(r.y ?? 0)
    const from = nearest({ x: ax + Number(s.x ?? 0), y: ay + Number(s.y ?? 0) })
    const to = nearest({ x: ax + Number(e.x ?? 0), y: ay + Number(e.y ?? 0) })
    if (from && to && from.id !== to.id) edges.push({ from: from.id, to: to.id })
  }
  return { nodes, edges }
}

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

type CanvasNodeData = { label: string; color: string; kind: ShapeKind }

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
            fill="var(--panel)"
            stroke={data.color}
            strokeWidth={1.5}
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

/* ---------- ks_diagram ↔ react flow ---------- */

const SIDE_POS: Record<string, string> = { t: 't', b: 'b', l: 'l', r: 'r' }

function toFlow(d: KsDiagram): { nodes: Node<CanvasNodeData>[]; edges: Edge[] } {
  const needLayout = d.nodes.some((n) => n.x === undefined || n.y === undefined)
  const pos = needLayout ? layout(d) : new Map()
  const nodes = d.nodes.map((n) => {
    const kind: ShapeKind = n.shape ?? 'box'
    const [dw, dh] = DEFAULT_SIZE[kind]
    return {
      id: n.id,
      type: 'shape' as const,
      position: { x: n.x ?? pos.get(n.id)?.x ?? 0, y: n.y ?? pos.get(n.id)?.y ?? 0 },
      width: n.w ?? dw,
      height: n.h ?? dh,
      zIndex: kind === 'frame' ? -1 : 0,
      data: { label: n.label ?? '', color: n.color ?? COLORS[0], kind },
    }
  })
  // side-less edges (agent diagrams) get geometrically sensible handles:
  // mostly-horizontal pairs connect left↔right, vertical pairs top↔bottom
  const byId = new Map(nodes.map((n) => [n.id, n]))
  const pickSides = (from: string, to: string): [string, string] => {
    const a = byId.get(from)
    const b = byId.get(to)
    if (!a || !b) return ['r', 'l']
    const dx = b.position.x - a.position.x
    const dy = b.position.y - a.position.y
    if (Math.abs(dx) >= Math.abs(dy)) return dx >= 0 ? ['r', 'l'] : ['l', 'r']
    return dy >= 0 ? ['b', 't'] : ['t', 'b']
  }
  const edges = d.edges.map((e, i) => {
    const [autoFrom, autoTo] = pickSides(e.from, e.to)
    return {
      id: e.id ?? `e${i}-${e.from}-${e.to}`,
      source: e.from,
      target: e.to,
      sourceHandle: e.fromSide && SIDE_POS[e.fromSide] ? e.fromSide : autoFrom,
      targetHandle: e.toSide && SIDE_POS[e.toSide] ? e.toSide : autoTo,
      label: e.label,
      type: 'label' as const,
    }
  })
  return { nodes, edges }
}

function fromFlow(nodes: Node<CanvasNodeData>[], edges: Edge[]): KsDiagram {
  return {
    nodes: nodes.map((n) => ({
      id: n.id,
      label: n.data.label,
      x: Math.round(n.position.x),
      y: Math.round(n.position.y),
      w: Math.round(n.width ?? n.measured?.width ?? 180),
      h: Math.round(n.height ?? n.measured?.height ?? 64),
      shape: n.data.kind,
      color: n.data.color,
    })),
    edges: edges.map((e) => ({
      id: e.id,
      from: e.source,
      to: e.target,
      label: typeof e.label === 'string' ? e.label : undefined,
      fromSide: e.sourceHandle ?? undefined,
      toSide: e.targetHandle ?? undefined,
    })),
  }
}

/* ---------- the editor ---------- */

function parseContent(content: string): KsDiagram {
  try {
    const parsed = JSON.parse(content)
    if (parsed?.ks_diagram) return parsed.ks_diagram as KsDiagram
    if (parsed?.document) return convertTldraw(parsed) // tldraw-era canvas
  } catch {
    // empty/new canvas
  }
  return { nodes: [], edges: [] }
}

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
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const dirty = useRef(false)
  useEffect(() => {
    epochRef.current = epoch
  }, [epoch])

  // debounced save through the gate, same as everything else
  const scheduleSave = useCallback(() => {
    dirty.current = true
    if (timer.current) clearTimeout(timer.current)
    timer.current = setTimeout(async () => {
      if (!dirty.current) return
      dirty.current = false
      const d = fromFlow(
        (nodesRef.current ?? []) as Node<CanvasNodeData>[],
        edgesRef.current ?? [],
      )
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
        onSaved()
      } catch (e) {
        console.error('canvas save failed', e)
      }
    }, 1200)
  }, [block.id, block.doc_id, onSaved])

  const nodesRef = useRef(nodes)
  nodesRef.current = nodes
  const edgesRef = useRef(edges)
  edgesRef.current = edges
  // any structural change schedules a save
  useEffect(() => {
    if (nodes !== initial.nodes || edges !== initial.edges) scheduleSave()
  }, [nodes, edges, initial, scheduleSave])

  const onConnect = useCallback(
    (c: Connection) => setEdges((es) => addEdge({ ...c, type: 'label' }, es)),
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
  }

  const tidy = () => {
    const d = fromFlow(nodes, edges)
    const pos = layout({ nodes: d.nodes.map((n) => ({ ...n, x: undefined, y: undefined })), edges: d.edges })
    setNodes((ns) =>
      ns.map((n) =>
        n.data.kind === 'frame' ? n : { ...n, position: pos.get(n.id) ?? n.position },
      ),
    )
    setTimeout(() => fitView({ padding: 0.15 }), 50)
  }

  return (
    <>
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
        <button onClick={recolor} title="cycle color of selection">🎨</button>
        <button onClick={tidy} title="auto-layout">✨</button>
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
    </>
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
