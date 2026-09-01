import { describe, expect, it } from 'vitest'
import * as Y from 'yjs'
import {
  DEFAULT_SIZE,
  KsDiagram,
  decorate,
  diffShapes,
  fromFlow,
  layout,
  mergeShapes,
  parseContent,
  serializeShapes,
  toFlow,
} from './convert'

const sample: KsDiagram = {
  nodes: [
    { id: 'a', label: 'Start', x: 0, y: 0, w: 170, h: 56, shape: 'pill', color: '#95c99b' },
    { id: 'b', label: 'Decide [[Doc]]', x: 300, y: 0, w: 170, h: 110, shape: 'diamond', color: '#d9b47a' },
    { id: 'c', label: 'Store', x: 300, y: 200, w: 150, h: 100, shape: 'cylinder', color: '#8b9dc3' },
  ],
  edges: [
    { id: 'e1', from: 'a', to: 'b', label: 'go', fromSide: 'r', toSide: 'l', arrow: 'end' },
    { id: 'e2', from: 'b', to: 'c', fromSide: 'b', toSide: 't', arrow: 'both', dash: true, color: '#d98a94' },
    { id: 'e3', from: 'c', to: 'a', fromSide: 'l', toSide: 'b', arrow: 'none' },
  ],
}

describe('ks_diagram ↔ React Flow', () => {
  it('round-trips ids, labels, positions, sizes, shapes and colors', () => {
    const flow = toFlow(sample)
    const back = fromFlow(flow.nodes, flow.edges)
    expect(back.nodes).toEqual(sample.nodes)
  })

  it('round-trips edge endpoints, sides, labels and arrow styles', () => {
    const flow = toFlow(sample)
    const back = fromFlow(flow.nodes, flow.edges)
    expect(back.edges.map((e) => [e.id, e.from, e.to, e.fromSide, e.toSide])).toEqual(
      sample.edges.map((e) => [e.id, e.from, e.to, e.fromSide, e.toSide]),
    )
    expect(back.edges.map((e) => e.arrow)).toEqual(['end', 'both', 'none'])
    expect(back.edges[0].label).toBe('go')
    expect(back.edges[1].dash).toBe(true)
    expect(back.edges[1].color).toBe('#d98a94')
    expect(back.edges[0].dash).toBeUndefined() // false collapses to absent
  })

  it('is idempotent: a second round trip is byte-identical', () => {
    const once = (() => {
      const f = toFlow(sample)
      return fromFlow(f.nodes, f.edges)
    })()
    const f2 = toFlow(once)
    expect(fromFlow(f2.nodes, f2.edges)).toEqual(once)
  })

  it('fills defaults: box shape, palette color, per-shape size, end arrow', () => {
    const d: KsDiagram = {
      nodes: [{ id: 'n', label: 'x', x: 1, y: 2 }, { id: 'm', label: 'y', x: 5, y: 6, shape: 'sticky' }],
      edges: [{ from: 'n', to: 'm' }],
    }
    const flow = toFlow(d)
    expect(flow.nodes[0].data.kind).toBe('box')
    expect([flow.nodes[0].width, flow.nodes[0].height]).toEqual(DEFAULT_SIZE.box)
    expect([flow.nodes[1].width, flow.nodes[1].height]).toEqual(DEFAULT_SIZE.sticky)
    expect(flow.nodes[0].data.color).toMatch(/^#/)
    expect(flow.edges[0].id).toBe('e0-n-m')
    expect(flow.edges[0].data).toMatchObject({ arrow: 'end', dash: false })
    expect(flow.edges[0].markerEnd).toBeDefined()
    expect(flow.edges[0].markerStart).toBeUndefined()
  })

  it('frames sit behind everything else', () => {
    const flow = toFlow({ nodes: [{ id: 'f', label: '', x: 0, y: 0, shape: 'frame' }], edges: [] })
    expect(flow.nodes[0].zIndex).toBe(-1)
  })

  it('picks geometric handle sides for side-less edges', () => {
    const d: KsDiagram = {
      nodes: [
        { id: 'l', label: '', x: 0, y: 0 },
        { id: 'r', label: '', x: 400, y: 10 },
        { id: 'd', label: '', x: 0, y: 300 },
      ],
      edges: [
        { from: 'l', to: 'r' }, // mostly horizontal, rightwards
        { from: 'r', to: 'l' }, // leftwards
        { from: 'l', to: 'd' }, // downwards
        { from: 'd', to: 'l' }, // upwards
        { from: 'l', to: 'r', fromSide: 't', toSide: 'b' }, // explicit sides win
        { from: 'l', to: 'r', fromSide: 'nope' }, // invalid side falls back
      ],
    }
    const sides = toFlow(d).edges.map((e) => [e.sourceHandle, e.targetHandle])
    expect(sides).toEqual([
      ['r', 'l'],
      ['l', 'r'],
      ['b', 't'],
      ['t', 'b'],
      ['t', 'b'],
      ['r', 'l'],
    ])
  })

  it('lays out agent diagrams that ship without coordinates', () => {
    const d: KsDiagram = {
      nodes: [{ id: 'a', label: 'a' }, { id: 'b', label: 'b' }, { id: 'c', label: 'c' }],
      edges: [{ from: 'a', to: 'b' }, { from: 'a', to: 'c' }],
    }
    const pos = layout(d)
    expect(pos.get('a')).toEqual({ x: 0, y: 0 })
    expect(pos.get('b')!.x).toBeGreaterThan(pos.get('a')!.x)
    expect(pos.get('c')!.x).toBe(pos.get('b')!.x)
    expect(pos.get('c')!.y).not.toBe(pos.get('b')!.y)
    // toFlow applies it, and the result survives a round trip as real coordinates
    const flow = toFlow(d)
    const back = fromFlow(flow.nodes, flow.edges)
    expect(back.nodes.map((n) => [n.x, n.y])).toEqual(['a', 'b', 'c'].map((id) => [pos.get(id)!.x, pos.get(id)!.y]))
  })

  it('layout tolerates cycles', () => {
    const d: KsDiagram = {
      nodes: [{ id: 'a', label: '' }, { id: 'b', label: '' }],
      edges: [{ from: 'a', to: 'b' }, { from: 'b', to: 'a' }],
    }
    const pos = layout(d)
    expect(pos.size).toBe(2)
  })

  it('decorate maps arrow kinds to markers and dash to a dasharray', () => {
    const base = { id: 'e', source: 'a', target: 'b' }
    const end = decorate({ ...base, data: { arrow: 'end' } })
    const both = decorate({ ...base, data: { arrow: 'both', dash: true, color: '#fff' } })
    const none = decorate({ ...base, data: { arrow: 'none' } })
    expect(end.markerEnd).toBeDefined()
    expect(end.markerStart).toBeUndefined()
    expect(both.markerEnd).toBeDefined()
    expect(both.markerStart).toBeDefined()
    expect((both.markerEnd as { color: string }).color).toBe('#fff')
    expect(both.style).toMatchObject({ stroke: '#fff', strokeDasharray: '6 4' })
    expect(none.markerEnd).toBeUndefined()
    expect(none.markerStart).toBeUndefined()
  })
})

describe('parseContent', () => {
  const empty = { nodes: [], edges: [] }
  it('reads {"ks_diagram": …}', () => {
    expect(parseContent(JSON.stringify({ ks_diagram: sample }))).toEqual(sample)
  })
  it('empty string → empty canvas', () => {
    expect(parseContent('')).toEqual(empty)
  })
  it('invalid JSON → empty canvas', () => {
    expect(parseContent('{not json')).toEqual(empty)
  })
  it('legacy tldraw {"document": …} → empty canvas (never released, not converted)', () => {
    const legacy = JSON.stringify({
      document: { store: { 'shape:x': { typeName: 'shape', type: 'geo', x: 1, y: 2, props: { w: 10, h: 10 } } } },
    })
    expect(parseContent(legacy)).toEqual(empty)
  })
  it('{} and unrelated JSON → empty canvas', () => {
    expect(parseContent('{}')).toEqual(empty)
    expect(parseContent('[1,2]')).toEqual(empty)
    expect(parseContent('null')).toEqual(empty)
  })
})

describe('live canvas: shape-level LWW', () => {
  const moved = (d: KsDiagram, id: string, x: number): KsDiagram => ({
    ...d,
    nodes: d.nodes.map((n) => (n.id === id ? { ...n, x } : n)),
  })

  it('diffShapes emits only the changed shape and the deletions', () => {
    const prev = serializeShapes(sample)
    const next = serializeShapes({
      nodes: moved(sample, 'b', 999).nodes.filter((n) => n.id !== 'c'),
      edges: sample.edges.filter((e) => e.id !== 'e2'),
    })
    const delta = diffShapes(prev, next)
    expect(delta.nodes.set.map(([k]) => k)).toEqual(['b'])
    expect(delta.nodes.del).toEqual(['c'])
    expect(delta.edges.set).toEqual([])
    expect(delta.edges.del).toEqual(['e2'])
  })

  it('diffShapes of identical maps is empty', () => {
    const m = serializeShapes(sample)
    const delta = diffShapes(m, serializeShapes(sample))
    expect(delta).toEqual({ nodes: { set: [], del: [] }, edges: { set: [], del: [] } })
  })

  it('mergeShapes rebuilds the diagram and drops malformed entries', () => {
    const m = serializeShapes(sample)
    m.nodes.set('junk', '{not json')
    const { diagram, pushed } = mergeShapes(m.nodes.entries(), m.edges.entries())
    expect(diagram.nodes.map((n) => n.id).sort()).toEqual(['a', 'b', 'c'])
    expect(diagram.edges).toEqual(sample.edges)
    expect(pushed.nodes.has('junk')).toBe(false)
    expect(pushed.nodes.size).toBe(3)
  })

  // Two peers over real Y.Maps (the same structure the daemon hosts): the
  // later write to a key wins, earlier loses, other keys are untouched,
  // deletions propagate.
  function peers() {
    const A = new Y.Doc()
    const B = new Y.Doc()
    const maps = (d: Y.Doc) => ({ nodes: d.getMap<string>('canvas_nodes'), edges: d.getMap<string>('canvas_edges') })
    const a = maps(A)
    const b = maps(B)
    const sync = () => {
      Y.applyUpdate(B, Y.encodeStateAsUpdate(A))
      Y.applyUpdate(A, Y.encodeStateAsUpdate(B))
    }
    const push = (m: ReturnType<typeof maps>, prev: ReturnType<typeof serializeShapes>, d: KsDiagram) => {
      const now = serializeShapes(d)
      const delta = diffShapes(prev, now)
      m.nodes.doc!.transact(() => {
        for (const [k, v] of delta.nodes.set) m.nodes.set(k, v)
        for (const k of delta.nodes.del) m.nodes.delete(k)
        for (const [k, v] of delta.edges.set) m.edges.set(k, v)
        for (const k of delta.edges.del) m.edges.delete(k)
      }, 'local')
      return now
    }
    const view = (m: ReturnType<typeof maps>) => mergeShapes(m.nodes.entries(), m.edges.entries()).diagram
    return { A, B, a, b, sync, push, view }
  }

  it('a later remote update to one shape wins; unrelated shapes are untouched', () => {
    const { a, b, sync, push, view } = peers()
    let lastA = push(a, serializeShapes({ nodes: [], edges: [] }), sample) // A seeds
    sync()
    let lastB = mergeShapes(b.nodes.entries(), b.edges.entries()).pushed
    // B moves 'b' after A's write
    lastB = push(b, lastB, moved(sample, 'b', 777))
    sync()
    const seenByA = view(a)
    expect(seenByA.nodes.find((n) => n.id === 'b')!.x).toBe(777)
    expect(seenByA.nodes.find((n) => n.id === 'a')).toEqual(sample.nodes[0])
    expect(seenByA.nodes.find((n) => n.id === 'c')).toEqual(sample.nodes[2])
    expect(seenByA.edges).toEqual(sample.edges)
    // and A's next diff against the merged state pushes nothing spurious
    lastA = mergeShapes(a.nodes.entries(), a.edges.entries()).pushed
    expect(diffShapes(lastA, serializeShapes(seenByA))).toEqual({
      nodes: { set: [], del: [] },
      edges: { set: [], del: [] },
    })
  })

  it('the older of two sequential writes to the same shape loses', () => {
    const { a, b, sync, push, view } = peers()
    push(a, serializeShapes({ nodes: [], edges: [] }), sample)
    sync()
    const lastB = mergeShapes(b.nodes.entries(), b.edges.entries()).pushed
    push(b, lastB, moved(sample, 'b', 111)) // older
    sync()
    const lastA = mergeShapes(a.nodes.entries(), a.edges.entries()).pushed
    push(a, lastA, moved(sample, 'b', 222)) // newer
    sync()
    expect(view(a).nodes.find((n) => n.id === 'b')!.x).toBe(222)
    expect(view(b).nodes.find((n) => n.id === 'b')!.x).toBe(222)
  })

  it('deletions propagate to the other peer', () => {
    const { a, b, sync, push, view } = peers()
    push(a, serializeShapes({ nodes: [], edges: [] }), sample)
    sync()
    const lastB = mergeShapes(b.nodes.entries(), b.edges.entries()).pushed
    push(b, lastB, {
      nodes: sample.nodes.filter((n) => n.id !== 'c'),
      edges: sample.edges.filter((e) => e.from !== 'c' && e.to !== 'c'),
    })
    sync()
    const seen = view(a)
    expect(seen.nodes.map((n) => n.id).sort()).toEqual(['a', 'b'])
    expect(seen.edges.map((e) => e.id)).toEqual(['e1'])
  })

  it('concurrent edits to different shapes both survive', () => {
    const { a, b, sync, push, view } = peers()
    push(a, serializeShapes({ nodes: [], edges: [] }), sample)
    sync()
    const lastA = mergeShapes(a.nodes.entries(), a.edges.entries()).pushed
    const lastB = mergeShapes(b.nodes.entries(), b.edges.entries()).pushed
    push(a, lastA, moved(sample, 'a', 10)) // offline, concurrent
    push(b, lastB, moved(sample, 'c', 20))
    sync()
    const seen = view(a)
    expect(seen.nodes.find((n) => n.id === 'a')!.x).toBe(10)
    expect(seen.nodes.find((n) => n.id === 'c')!.x).toBe(20)
    expect(view(b)).toEqual(seen)
  })
})
