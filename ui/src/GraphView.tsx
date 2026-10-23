// Graph view (5.10): read-only, never writes. Nodes = docs, edges =
// resolved wikilinks, tint = tending principal — the visualization nothing
// else can draw because nothing else has principal-level provenance. Lapis
// page frame (no artboard): title and lead, a legend of who tends what as
// chips, then the graph in a card that follows the theme (colours are read
// from the tokens, so light and dark both hold).

import { useEffect, useMemo, useRef, useState } from 'react'
import ForceGraph2D from 'react-force-graph-2d'
import { api } from './types'
import { useTheme, Card, EmptyState, Icon, SkeletonLines } from './lapis'
import './screens/places.css'

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

/** Read a token's current value (the canvas cannot use var()). */
function token(name: string, fallback: string): string {
  try {
    const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim()
    return v || fallback
  } catch {
    return fallback
  }
}

/** The tenders worth a colour: most docs first, at most six (the six
 * workspace tokens); the rest share the neutral ink. */
export function tenderPalette(nodes: { tender: string | null }[], max = 6): string[] {
  const n = new Map<string, number>()
  for (const d of nodes) if (d.tender) n.set(d.tender, (n.get(d.tender) ?? 0) + 1)
  return [...n.entries()]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .slice(0, max)
    .map(([t]) => t)
}

export default function GraphView({ onOpenDoc }: { onOpenDoc: (id: string) => void }) {
  const [raw, setRaw] = useState<GraphData | null>(null)
  const [failed, setFailed] = useState(false)
  const [showLoose, setShowLoose] = useState(false)
  const [width, setWidth] = useState(800)
  const wrapRef = useRef<HTMLDivElement>(null)
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const fgRef = useRef<any>(null)
  // fit the graph to the card once, when it first settles (not after a drag)
  const fitted = useRef(false)
  const { theme } = useTheme()

  useEffect(() => {
    api<GraphData>('/api/graph')
      .then((g) => setRaw({ nodes: g?.nodes ?? [], links: g?.links ?? [] }))
      .catch(() => setFailed(true))
  }, [])

  // keep it legible: drop fully isolated docs (no links, no tags) unless asked
  const data = useMemo(() => {
    if (!raw) return null
    if (showLoose) return raw
    const linked = new Set<string>()
    for (const l of raw.links) {
      linked.add(String(typeof l.source === 'object' ? (l.source as GNode).id : l.source))
      linked.add(String(typeof l.target === 'object' ? (l.target as GNode).id : l.target))
    }
    return { nodes: raw.nodes.filter((n) => linked.has(n.id) || n.tags.length > 0), links: raw.links }
  }, [raw, showLoose])

  useEffect(() => {
    const el = wrapRef.current
    if (!el) return
    const ro = new ResizeObserver(() => setWidth(Math.max(280, el.clientWidth)))
    ro.observe(el)
    setWidth(Math.max(280, el.clientWidth))
    return () => ro.disconnect()
  }, [data])

  const tenders = useMemo(() => tenderPalette(raw?.nodes ?? []), [raw])
  // the theme flips the tokens: recompute the colours with it
  const colors = useMemo(() => {
    const pal = tenders.map((_, i) => token(`--ws-${i + 1}`, '#3346d3'))
    return {
      byTender: new Map(tenders.map((t, i) => [t, pal[i]])),
      none: token('--ink3', '#5e6779'),
      line: token('--line2', 'rgba(20,26,45,.17)'),
      label: token('--ink2', '#454d5e'),
      bg: token('--card', '#ffffff'),
      font: token('--font-ui', 'sans-serif'),
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tenders, theme])
  const height = typeof window !== 'undefined' && window.innerWidth < 720 ? Math.max(360, window.innerHeight * 0.6) : Math.max(460, (typeof window !== 'undefined' ? window.innerHeight : 900) - 300)
  const hidden = raw && data ? raw.nodes.length - data.nodes.length : 0

  return (
    <div className="lp-page lp-place lp-graph">
      <header className="lp-page-head">
        <div className="lp-page-head-text">
          <div className="lp-page-kicker">
            <span className="lp-pill">
              <Icon name="graph" size={13} />
              Graph
            </span>
            {data && (
              <span className="muted">
                {data.nodes.length} doc{data.nodes.length === 1 ? '' : 's'} · {data.links.length} link{data.links.length === 1 ? '' : 's'}
              </span>
            )}
          </div>
          <h1 className="lp-page-title">Graph</h1>
          <p className="lp-page-lead">Docs and the links between them, coloured by who tends each one. Click a doc to open it.</p>
        </div>
      </header>

      {failed ? (
        <Card>
          <EmptyState icon="graph" title="The graph could not load" compact>
            Try again in a moment.
          </EmptyState>
        </Card>
      ) : !data ? (
        <Card>
          <SkeletonLines lines={4} />
        </Card>
      ) : (
        <>
          <div className="lp-graph-legend" aria-label="Who tends what">
            {tenders.map((t) => (
              <span key={t} className="lp-graph-key">
                <i style={{ background: colors.byTender.get(t) }} aria-hidden />
                <span className={t.includes(':') ? 'mono' : ''}>{t}</span>
              </span>
            ))}
            <span className="lp-graph-key">
              <i style={{ background: colors.none }} aria-hidden />
              {tenders.length ? 'anyone else' : 'untended'}
            </span>
            {raw && (hidden > 0 || showLoose) && (
              <label className="lp-graph-loose">
                <input type="checkbox" checked={showLoose} onChange={(e) => setShowLoose(e.target.checked)} />
                Show docs with no links {hidden > 0 && !showLoose ? `(${hidden})` : ''}
              </label>
            )}
          </div>
          {data.nodes.length === 0 ? (
            <Card>
              <EmptyState icon="link" title="No links yet" compact>
                Link docs with [[Title]] and they show up here, joined.
              </EmptyState>
            </Card>
          ) : (
            <div ref={wrapRef} className="lp-card lp-card-flush lp-graph-canvas">
              <ForceGraph2D
                key={theme}
                ref={fgRef}
                onEngineStop={() => {
                  if (fitted.current) return
                  fitted.current = true
                  fgRef.current?.zoomToFit?.(400, 48)
                }}
                graphData={data}
                width={width}
                height={height}
                backgroundColor={colors.bg}
                nodeId="id"
                nodeLabel={(n: GNode) => `${n.title}${n.tender ? ` · ${n.tender}` : ''}${n.tags.length ? ` · #${n.tags.join(' #')}` : ''}`}
                nodeColor={(n: GNode) => (n.tender ? (colors.byTender.get(n.tender) ?? colors.none) : colors.none)}
                nodeRelSize={4}
                linkColor={() => colors.line}
                linkWidth={1}
                onNodeClick={(n: GNode) => onOpenDoc(n.id)}
                cooldownTicks={120}
                nodeCanvasObjectMode={() => 'after'}
                nodeCanvasObject={(n: GNode, ctx, scale) => {
                  if (scale < 1.2) return
                  ctx.font = `${11 / scale}px ${colors.font}`
                  ctx.fillStyle = colors.label
                  ctx.textAlign = 'center'
                  ctx.fillText(n.title.slice(0, 28), n.x!, n.y! + 8 / scale + 4)
                }}
              />
            </div>
          )}
        </>
      )}
    </div>
  )
}
