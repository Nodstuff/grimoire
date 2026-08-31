// Graph view (5.10): read-only, never writes. Nodes = docs, edges =
// resolved wikilinks, tint = tending principal — the visualization nothing
// else can draw because nothing else has principal-level provenance.

import { useEffect, useRef, useState } from 'react'
import ForceGraph2D from 'react-force-graph-2d'
import { api } from './types'

interface GNode {
  id: string
  title: string
  tender: string | null
  tags: string[]
  x?: number
  y?: number
}

interface GraphData {
  nodes: GNode[]
  links: { source: string; target: string }[]
}

const TENDER_COLORS: Record<string, string> = {
  tom: '#8b9dc3',
  claude: '#95c99b',
  tagging: '#d9b47a',
  reviewer: '#d98a94',
}

function colorFor(tender: string | null): string {
  if (!tender) return '#3a3a46'
  return TENDER_COLORS[tender] ?? '#b48ead'
}

export default function GraphView({ onOpenDoc }: { onOpenDoc: (id: string) => void }) {
  const [data, setData] = useState<GraphData | null>(null)
  const [size, setSize] = useState({ w: window.innerWidth, h: window.innerHeight })
  const wrapRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    api<GraphData>('/api/graph')
      .then((g) => {
        // keep it legible: drop fully isolated folder docs (no links, no tags)
        const linked = new Set<string>()
        for (const l of g.links) {
          linked.add(l.source)
          linked.add(l.target)
        }
        setData({
          nodes: g.nodes.filter((n) => linked.has(n.id) || n.tags.length > 0),
          links: g.links,
        })
      })
      .catch(console.error)
  }, [])

  useEffect(() => {
    const onResize = () => setSize({ w: window.innerWidth, h: window.innerHeight })
    window.addEventListener('resize', onResize)
    return () => window.removeEventListener('resize', onResize)
  }, [])

  if (!data) return <div className="empty">…</div>

  return (
    <div ref={wrapRef} className="graph-wrap">
      <div className="graph-legend">
        {Object.entries(TENDER_COLORS).map(([who, c]) => (
          <span key={who}>
            <i style={{ background: c }} /> {who}
          </span>
        ))}
      </div>
      <ForceGraph2D
        graphData={data}
        width={size.w}
        height={size.h}
        backgroundColor="#101014"
        nodeId="id"
        nodeLabel={(n: GNode) => `${n.title}${n.tags.length ? ` · ${n.tags.join(', ')}` : ''}`}
        nodeColor={(n: GNode) => colorFor(n.tender)}
        nodeRelSize={4}
        linkColor={() => 'rgba(139, 157, 195, 0.18)'}
        linkWidth={1}
        onNodeClick={(n: GNode) => onOpenDoc(n.id)}
        cooldownTicks={120}
        nodeCanvasObjectMode={() => 'after'}
        nodeCanvasObject={(n: GNode, ctx, scale) => {
          if (scale < 1.2) return
          ctx.font = `${11 / scale}px -apple-system, sans-serif`
          ctx.fillStyle = 'rgba(214, 214, 221, 0.75)'
          ctx.textAlign = 'center'
          ctx.fillText(n.title.slice(0, 28), n.x!, n.y! + 8 / scale + 4)
        }}
      />
    </div>
  )
}
