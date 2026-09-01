// Pure canvas logic, no React: the ks_diagram storage format, its mapping to
// and from React Flow nodes/edges, the fallback layout for agent-written
// diagrams without coordinates, and the per-shape serialisation used by live
// canvas sessions (shape-level LWW over Y.Maps, P2.5). CanvasBlock.tsx renders;
// this file is what the unit tests exercise.

import { MarkerType, type Edge, type Node } from '@xyflow/react'

/* ---------- storage format ---------- */

export type ShapeKind =
  | 'box'
  | 'pill'
  | 'ellipse'
  | 'diamond'
  | 'parallelogram'
  | 'hexagon'
  | 'cylinder'
  | 'document'
  | 'cloud'
  | 'actor'
  | 'triangle'
  | 'sticky'
  | 'text'
  | 'frame'

export interface KsNode {
  id: string
  label: string
  x?: number
  y?: number
  w?: number
  h?: number
  shape?: ShapeKind
  color?: string
}

export type ArrowKind = 'none' | 'end' | 'both'

export interface KsEdge {
  id?: string
  from: string
  to: string
  label?: string
  fromSide?: string
  toSide?: string
  arrow?: ArrowKind
  dash?: boolean
  color?: string
}

export interface KsDiagram {
  nodes: KsNode[]
  edges: KsEdge[]
}

export const COLORS = ['#8b9dc3', '#95c99b', '#d9b47a', '#d98a94', '#a88bd4', '#7bc4c4', '#6b6b7b']
export const DEFAULT_SIZE: Record<ShapeKind, [number, number]> = {
  box: [180, 64],
  pill: [170, 56],
  ellipse: [160, 90],
  diamond: [170, 110],
  parallelogram: [190, 70],
  hexagon: [180, 80],
  cylinder: [150, 100],
  document: [170, 90],
  cloud: [190, 110],
  actor: [80, 110],
  triangle: [150, 110],
  sticky: [160, 140],
  text: [160, 40],
  frame: [420, 300],
}

export type CanvasNodeData = { label: string; color: string; kind: ShapeKind }
export type CanvasEdgeData = { arrow?: ArrowKind; dash?: boolean; color?: string }
export type FlowNode = Node<CanvasNodeData>

/* ---------- layered fallback layout (agent diagrams ship without x/y) ---------- */

export function layout(d: KsDiagram): Map<string, { x: number; y: number }> {
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

/* ---------- edge decoration ---------- */

/** Derive React Flow marker/style props from our edge data. */
export function decorate(e: Edge): Edge {
  const d = (e.data ?? {}) as CanvasEdgeData
  const arrow = d.arrow ?? 'end'
  const color = d.color ?? '#6b6b7b'
  const marker = { type: MarkerType.ArrowClosed, width: 16, height: 16, color }
  return {
    ...e,
    markerEnd: arrow === 'end' || arrow === 'both' ? marker : undefined,
    markerStart: arrow === 'both' ? marker : undefined,
    style: {
      stroke: d.color,
      strokeDasharray: d.dash ? '6 4' : undefined,
    },
  }
}

/* ---------- ks_diagram ↔ react flow ---------- */

const SIDE_POS: Record<string, string> = { t: 't', b: 'b', l: 'l', r: 'r' }

export function toFlow(d: KsDiagram): { nodes: FlowNode[]; edges: Edge[] } {
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
    return decorate({
      id: e.id ?? `e${i}-${e.from}-${e.to}`,
      source: e.from,
      target: e.to,
      sourceHandle: e.fromSide && SIDE_POS[e.fromSide] ? e.fromSide : autoFrom,
      targetHandle: e.toSide && SIDE_POS[e.toSide] ? e.toSide : autoTo,
      label: e.label,
      type: 'label' as const,
      data: { arrow: e.arrow ?? 'end', dash: e.dash ?? false, color: e.color },
    })
  })
  return { nodes, edges }
}

export function fromFlow(nodes: FlowNode[], edges: Edge[]): KsDiagram {
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
    edges: edges.map((e) => {
      const d = (e.data ?? {}) as CanvasEdgeData
      return {
        id: e.id,
        from: e.source,
        to: e.target,
        label: typeof e.label === 'string' ? e.label : undefined,
        fromSide: e.sourceHandle ?? undefined,
        toSide: e.targetHandle ?? undefined,
        arrow: d.arrow ?? 'end',
        dash: d.dash || undefined,
        color: d.color,
      }
    }),
  }
}

/** Block content → diagram. Anything that is not `{"ks_diagram": …}` (empty,
 * invalid JSON, the never-released tldraw `{"document": …}` era) is an empty
 * canvas. */
export function parseContent(content: string): KsDiagram {
  try {
    const parsed = JSON.parse(content)
    if (parsed?.ks_diagram) return parsed.ks_diagram as KsDiagram
  } catch {
    // empty/new canvas
  }
  return { nodes: [], edges: [] }
}

/* ---------- live canvas: per-shape LWW over Y.Maps ----------
 * Shapes live in two maps (canvas_nodes / canvas_edges) keyed by id, values =
 * JSON strings. Y.Map gives last-writer-wins per key; these helpers are the
 * pure halves around it: serialise a diagram to shape maps, diff two shape
 * maps into the minimal set/delete ops to push, and rebuild a diagram from
 * whatever the maps currently hold. */

export interface ShapeMaps {
  nodes: Map<string, string>
  edges: Map<string, string>
}

export function serializeShapes(d: KsDiagram): ShapeMaps {
  return {
    nodes: new Map(d.nodes.map((n) => [n.id, JSON.stringify(n)])),
    edges: new Map(d.edges.map((e) => [e.id ?? '', JSON.stringify(e)])),
  }
}

export interface MapDelta {
  set: [string, string][]
  del: string[]
}

function diffOne(prev: Map<string, string>, now: Map<string, string>): MapDelta {
  const set: [string, string][] = []
  const del: string[] = []
  now.forEach((v, k) => {
    if (prev.get(k) !== v) set.push([k, v])
  })
  prev.forEach((_, k) => {
    if (!now.has(k)) del.push(k)
  })
  return { set, del }
}

/** Ops to move the shared maps from what we last pushed to what we have now. */
export function diffShapes(prev: ShapeMaps, now: ShapeMaps): { nodes: MapDelta; edges: MapDelta } {
  return { nodes: diffOne(prev.nodes, now.nodes), edges: diffOne(prev.edges, now.edges) }
}

/** Rebuild the diagram from the shared maps; malformed entries are dropped.
 * Returns the maps as read so the next diff is against the merged state. */
export function mergeShapes(
  nodes: Iterable<[string, string]>,
  edges: Iterable<[string, string]>,
): { diagram: KsDiagram; pushed: ShapeMaps } {
  const kd: KsDiagram = { nodes: [], edges: [] }
  const np = new Map<string, string>()
  const ep = new Map<string, string>()
  for (const [k, v] of nodes) {
    try {
      kd.nodes.push(JSON.parse(v))
      np.set(k, v)
    } catch {}
  }
  for (const [k, v] of edges) {
    try {
      kd.edges.push(JSON.parse(v))
      ep.set(k, v)
    } catch {}
  }
  return { diagram: kd, pushed: { nodes: np, edges: ep } }
}
