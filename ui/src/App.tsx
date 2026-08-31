import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import DocEditor from './editor/DocEditor'
import GraphView from './GraphView'
import Gardeners from './Gardeners'
import CanvasBlock from './CanvasBlock'
import {
  api,
  Block,
  Doc,
  DocTree,
  BlockNode,
  HistoryRow,
  QueueRow,
  SearchHit,
  GardenerRun,
} from './types'

type View =
  | { kind: 'doc'; id: string }
  | { kind: 'review' }
  | { kind: 'runs' }
  | { kind: 'graph' }
  | { kind: 'home' }
type Palette = null | 'commands' | 'search' | 'newdoc' | 'newcanvas'

export default function App() {
  const [view, setView] = useState<View>({ kind: 'home' })
  const [docs, setDocs] = useState<Doc[]>([])
  const [treeOpen, setTreeOpen] = useState(false)
  const [palette, setPalette] = useState<Palette>(null)
  const [queueCount, setQueueCount] = useState(0)

  const refreshQueue = useCallback(() => {
    Promise.all([
      api<QueueRow[]>('/api/queue').then((q) => q.length).catch(() => 0),
      api<{ block: { id: string } }[]>('/api/flags').then((f) => f.length).catch(() => 0),
    ]).then(([q, f]) => setQueueCount(q + f))
  }, [])

  useEffect(() => {
    api<Doc[]>('/api/docs').then(setDocs).catch(console.error)
    refreshQueue()
  }, [refreshQueue])

  // hot reload: poll the daemon's UI build stamp; a deploy reloads the app
  // (deferred while an editor has unsaved changes)
  useEffect(() => {
    let build: number | null = null
    const t = setInterval(async () => {
      try {
        const r = await api<{ build: number }>('/api/buildinfo')
        if (build === null) build = r.build
        else if (r.build !== build && !document.querySelector('.save-state.dirty, .save-state.saving')) {
          location.reload()
        }
      } catch {
        // daemon restarting mid-deploy — next tick catches the new build
      }
    }, 3000)
    return () => clearInterval(t)
  }, [])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey
      if (mod && e.key === 'k') {
        e.preventDefault()
        setPalette((p) => (p === 'commands' ? null : 'commands'))
      } else if (mod && e.key === 't') {
        e.preventDefault()
        setTreeOpen((t) => !t)
      } else if (mod && e.key === 'n') {
        e.preventDefault()
        setPalette((p) => (p === 'newdoc' ? null : 'newdoc'))
      } else if (mod && e.key === 'r') {
        e.preventDefault()
        location.reload()
      } else if (mod && e.key === 'w') {
        e.preventDefault()
        setView({ kind: 'home' })
      } else if (mod && e.key === 's' && !e.shiftKey) {
        e.preventDefault()
        setPalette((p) => (p === 'search' ? null : 'search'))
      } else if (e.key === 'Escape') {
        setPalette(null)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  const openDoc = useCallback((id: string) => {
    setView({ kind: 'doc', id })
    setPalette(null)
  }, [])

  return (
    <div className="app">
      {treeOpen && (
        <DocTreeNav
          docs={docs}
          selected={view.kind === 'doc' ? view.id : null}
          onSelect={openDoc}
          onClose={() => setTreeOpen(false)}
          onChanged={() => api<Doc[]>('/api/docs').then(setDocs).catch(() => {})}
        />
      )}
      <main className="stage" onClick={() => palette && setPalette(null)}>
        {view.kind === 'home' && (
          <div className="home">
            <div className="home-mark">◈</div>
            <div className="home-hints">
              <span><kbd>⌘K</kbd> commands</span>
              <span><kbd>⌘S</kbd> search</span>
              <span><kbd>⌘T</kbd> tree</span>
              <span><kbd>⌘N</kbd> new doc</span>
            </div>
          </div>
        )}
        {view.kind === 'doc' && <DocView docId={view.id} onOpenDoc={openDoc} docs={docs} />}
        {view.kind === 'review' && (
          <ReviewQueue
            onChange={setQueueCount}
            onOpenDoc={openDoc}
          />
        )}
        {view.kind === 'runs' && <Gardeners />}
        {view.kind === 'graph' && <GraphView onOpenDoc={openDoc} />}
      </main>

      {queueCount > 0 && view.kind !== 'review' && (
        <button className="queue-chip" onClick={() => setView({ kind: 'review' })}>
          {queueCount} to review
        </button>
      )}

      {palette === 'commands' && (
        <CommandPalette
          docs={docs}
          queueCount={queueCount}
          onOpenDoc={openDoc}
          onAction={(a) => {
            if (a === 'review') setView({ kind: 'review' })
            if (a === 'runs') setView({ kind: 'runs' })
            if (a === 'graph') setView({ kind: 'graph' })
            if (a === 'tree') setTreeOpen((t) => !t)
            if (a === 'home') setView({ kind: 'home' })
            if (a === 'newdoc') {
              setPalette('newdoc')
              return
            }
            if (a === 'newcanvas') {
              setPalette('newcanvas')
              return
            }
            setPalette(null)
          }}
          onClose={() => setPalette(null)}
        />
      )}
      {palette === 'search' && <SearchPalette onOpenDoc={openDoc} onClose={() => setPalette(null)} />}
      {(palette === 'newdoc' || palette === 'newcanvas') && (
        <NewDocPalette
          canvas={palette === 'newcanvas'}
          onCreated={(id) => {
            api<Doc[]>('/api/docs').then(setDocs).catch(() => {})
            openDoc(id)
          }}
          onClose={() => setPalette(null)}
        />
      )}
    </div>
  )
}

/* ---------- palettes ---------- */

function PaletteShell({
  children,
  onClose,
}: {
  children: React.ReactNode
  onClose: () => void
}) {
  return (
    <div className="palette-backdrop" onMouseDown={onClose}>
      <div className="palette" onMouseDown={(e) => e.stopPropagation()}>
        {children}
      </div>
    </div>
  )
}

function CommandPalette({
  docs,
  queueCount,
  onOpenDoc,
  onAction,
  onClose,
}: {
  docs: Doc[]
  queueCount: number
  onOpenDoc: (id: string) => void
  onAction: (a: 'review' | 'runs' | 'tree' | 'home' | 'newdoc' | 'newcanvas' | 'graph') => void
  onClose: () => void
}) {
  const [q, setQ] = useState('')
  const [sel, setSel] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  useEffect(() => inputRef.current?.focus(), [])

  type Item = { label: string; hint?: string; run: () => void }
  const commands: Item[] = [
    { label: `Review queue`, hint: queueCount ? `${queueCount} open` : 'empty', run: () => onAction('review') },
    { label: 'New doc…', hint: '⌘N', run: () => onAction('newdoc') },
    { label: 'New canvas…', run: () => onAction('newcanvas') },
    { label: 'Gardeners', run: () => onAction('runs') },
    { label: 'Graph view', run: () => onAction('graph') },
    { label: 'Toggle file tree', hint: '⌘T', run: () => onAction('tree') },
    { label: 'Home', run: () => onAction('home') },
  ]

  const items: Item[] = useMemo(() => {
    const needle = q.trim().toLowerCase()
    const cmds = commands.filter((c) => !needle || c.label.toLowerCase().includes(needle))
    const docItems = (needle
      ? docs.filter((d) => fuzzyMatch(needle, d.title.toLowerCase()))
      : docs.slice(0, 0)
    )
      .slice(0, 12)
      .map((d) => ({
        label: d.is_canvas ? `▨ ${d.title}` : d.title,
        hint: d.is_canvas ? 'canvas' : 'doc',
        run: () => onOpenDoc(d.id),
      }))
    return [...cmds, ...docItems]
  }, [q, docs, queueCount])

  useEffect(() => setSel(0), [q])

  return (
    <PaletteShell onClose={onClose}>
      <input
        ref={inputRef}
        placeholder="Type a command or doc name…"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown') setSel((s) => Math.min(s + 1, items.length - 1))
          if (e.key === 'ArrowUp') setSel((s) => Math.max(s - 1, 0))
          if (e.key === 'Enter') items[sel]?.run()
        }}
      />
      <div className="palette-list">
        {items.map((it, i) => (
          <div
            key={it.label + i}
            className={`palette-item ${i === sel ? 'sel' : ''}`}
            onMouseEnter={() => setSel(i)}
            onClick={() => it.run()}
          >
            <span>{it.label}</span>
            {it.hint && <span className="hint">{it.hint}</span>}
          </div>
        ))}
      </div>
    </PaletteShell>
  )
}

function fuzzyMatch(needle: string, hay: string): boolean {
  let i = 0
  for (const c of hay) {
    if (c === needle[i]) i++
    if (i === needle.length) return true
  }
  return false
}

function SearchPalette({
  onOpenDoc,
  onClose,
}: {
  onOpenDoc: (id: string) => void
  onClose: () => void
}) {
  const [q, setQ] = useState('')
  const [hits, setHits] = useState<SearchHit[]>([])
  const [sel, setSel] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  useEffect(() => inputRef.current?.focus(), [])

  useEffect(() => {
    if (q.trim().length < 2) {
      setHits([])
      return
    }
    const t = setTimeout(() => {
      api<SearchHit[]>(`/api/search?q=${encodeURIComponent(q)}`)
        .then(setHits)
        .catch(() => setHits([]))
    }, 120)
    return () => clearTimeout(t)
  }, [q])

  useEffect(() => setSel(0), [hits])

  return (
    <PaletteShell onClose={onClose}>
      <input
        ref={inputRef}
        placeholder="Search everything… (typos fine)"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown') setSel((s) => Math.min(s + 1, hits.length - 1))
          if (e.key === 'ArrowUp') setSel((s) => Math.max(s - 1, 0))
          if (e.key === 'Enter' && hits[sel]) onOpenDoc(hits[sel].block.doc_id)
        }}
      />
      <div className="palette-list">
        {hits.slice(0, 12).map((h, i) => (
          <div
            key={h.block.id}
            className={`palette-item ${i === sel ? 'sel' : ''}`}
            onMouseEnter={() => setSel(i)}
            onClick={() => onOpenDoc(h.block.doc_id)}
          >
            <div className="hit-body">
              <span className="hit-doc">{h.doc_title}</span>
              <span className="hit-text">{h.block.content.slice(0, 110)}</span>
            </div>
          </div>
        ))}
        {q.length >= 2 && hits.length === 0 && <div className="palette-empty">no hits</div>}
      </div>
    </PaletteShell>
  )
}

function NewDocPalette({
  canvas = false,
  onCreated,
  onClose,
}: {
  canvas?: boolean
  onCreated: (id: string) => void
  onClose: () => void
}) {
  const [title, setTitle] = useState('')
  const inputRef = useRef<HTMLInputElement>(null)
  useEffect(() => inputRef.current?.focus(), [])

  const create = async () => {
    if (!title.trim()) return
    const d = await api<Doc>('/api/docs', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ title: title.trim() }),
    })
    if (canvas) {
      await api('/api/propose', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          doc_id: d.id,
          base_epoch: 0,
          ops: [
            {
              kind: {
                op: 'insert',
                block_id: crypto.randomUUID(),
                parent_id: null,
                order_key: 'i',
                block_type: 'canvas_scene',
                content: '{}',
                refers_to: null,
              },
              source_refs: ['canvas:created'],
            },
          ],
        }),
      })
    }
    onCreated(d.id)
  }

  return (
    <PaletteShell onClose={onClose}>
      <input
        ref={inputRef}
        placeholder={canvas ? 'New canvas title…' : 'New doc title…'}
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') create().catch((err) => alert(String(err)))
        }}
      />
      <div className="palette-empty">Enter to create</div>
    </PaletteShell>
  )
}

/* ---------- tree ---------- */

type Drop = { id: string; mode: 'into' | 'before' | 'after' } | null

function DocTreeNav({
  docs,
  selected,
  onSelect,
  onClose,
  onChanged,
}: {
  docs: Doc[]
  selected: string | null
  onSelect: (id: string) => void
  onClose: () => void
  onChanged: () => void
}) {
  const childrenOf = useMemo(() => {
    const m = new Map<string | null, Doc[]>()
    for (const d of docs) {
      const k = d.parent_id
      if (!m.has(k)) m.set(k, [])
      m.get(k)!.push(d)
    }
    return m
  }, [docs])

  const [openDirs, setOpenDirs] = useState<Set<string>>(new Set())
  const [dragging, setDragging] = useState<string | null>(null)
  const [drop, setDrop] = useState<Drop>(null)

  const doMove = async (dragged: string, target: Doc, mode: 'into' | 'before' | 'after') => {
    if (dragged === target.id) return
    let parent: string | null
    let sortKey: string | null
    if (mode === 'into') {
      parent = target.id
      const kids = childrenOf.get(target.id) ?? []
      const last = kids[kids.length - 1]
      sortKey = keyBetween(last?.sort_key ?? null, null)
      setOpenDirs((s) => new Set(s).add(target.id))
    } else {
      parent = target.parent_id
      const siblings = (childrenOf.get(parent) ?? []).filter((d) => d.id !== dragged)
      const i = siblings.findIndex((d) => d.id === target.id)
      const before = mode === 'before' ? siblings[i - 1] : siblings[i]
      const after = mode === 'before' ? siblings[i] : siblings[i + 1]
      sortKey = keyBetween(before?.sort_key ?? null, after?.sort_key ?? null)
    }
    await api(`/api/doc/${dragged}/move`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ parent_id: parent, sort_key: sortKey }),
    }).catch((e) => alert(String(e)))
    onChanged()
  }

  // window.confirm is a silent no-op in Tauri's webview — arm-then-confirm
  // inline instead: first click arms the ×, second click within 2.5s deletes
  const [armed, setArmed] = useState<string | null>(null)
  const armTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const doDelete = async (d: Doc) => {
    if (armed !== d.id) {
      setArmed(d.id)
      if (armTimer.current) clearTimeout(armTimer.current)
      armTimer.current = setTimeout(() => setArmed(null), 2500)
      return
    }
    setArmed(null)
    await api(`/api/doc/${d.id}/delete`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: '{}',
    }).catch((e) => console.error(e))
    onChanged()
  }

  const renderLevel = (parent: string | null, depth: number): React.ReactNode =>
    (childrenOf.get(parent) ?? []).map((d) => {
      const isDir = !!childrenOf.get(d.id)?.length
      const isOpen = openDirs.has(d.id)
      const dropHere = drop?.id === d.id ? drop.mode : null
      return (
        <div key={d.id}>
          <div
            className={[
              'tree-item',
              selected === d.id ? 'sel' : '',
              dragging === d.id ? 'dragging' : '',
              dropHere === 'into' ? 'drop-into' : '',
              dropHere === 'before' ? 'drop-before' : '',
              dropHere === 'after' ? 'drop-after' : '',
            ].join(' ')}
            style={{ paddingLeft: 8 }}
            draggable
            onDragStart={(e) => {
              setDragging(d.id)
              e.dataTransfer.effectAllowed = 'move'
            }}
            onDragEnd={() => {
              setDragging(null)
              setDrop(null)
            }}
            onDragOver={(e) => {
              if (!dragging || dragging === d.id) return
              e.preventDefault()
              const r = e.currentTarget.getBoundingClientRect()
              const y = (e.clientY - r.top) / r.height
              setDrop({ id: d.id, mode: y < 0.3 ? 'before' : y > 0.7 ? 'after' : 'into' })
            }}
            onDragLeave={() => setDrop((cur) => (cur?.id === d.id ? null : cur))}
            onDrop={(e) => {
              e.preventDefault()
              if (dragging && drop?.id === d.id) doMove(dragging, d, drop.mode)
              setDrop(null)
              setDragging(null)
            }}
            onClick={() => {
              if (isDir)
                setOpenDirs((s) => {
                  const n = new Set(s)
                  if (n.has(d.id)) n.delete(d.id)
                  else n.add(d.id)
                  return n
                })
              onSelect(d.id)
            }}
          >
            <span className={`tree-icon ${d.is_canvas ? 'canvas' : ''}`}>
              {isDir ? (
                <svg width="10" height="10" viewBox="0 0 10 10" className={isOpen ? 'chev open' : 'chev'}>
                  <path d="M3 1.5 L7 5 L3 8.5" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
                </svg>
              ) : d.is_canvas ? (
                '▨'
              ) : (
                ''
              )}
            </span>
            <span className="tree-title">{d.title}</span>
            <button
              className={`tree-delete ${armed === d.id ? 'armed' : ''}`}
              title={armed === d.id ? 'click again to delete' : 'delete'}
              onClick={(e) => {
                e.stopPropagation()
                doDelete(d)
              }}
            >
              {armed === d.id ? 'sure?' : '×'}
            </button>
          </div>
          {isDir && isOpen && (
            <div className="tree-children">{renderLevel(d.id, depth + 1)}</div>
          )}
        </div>
      )
    })

  return (
    <aside className="sidebar">
      <div className="sidebar-head">
        <span>files</span>
        <button onClick={onClose}>⌘T</button>
      </div>
      <div
        className="tree-root"
        onDragOver={(e) => {
          // dropping on empty space = move to root
          if (dragging && e.target === e.currentTarget) e.preventDefault()
        }}
        onDrop={(e) => {
          if (dragging && e.target === e.currentTarget) {
            api(`/api/doc/${dragging}/move`, {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ parent_id: null, sort_key: null }),
            }).then(onChanged)
          }
        }}
      >
        {renderLevel(null, 0)}
      </div>
    </aside>
  )
}

const DIGITS = '0123456789abcdefghijklmnopqrstuvwxyz'
function keyBetween(a: string | null, b: string | null): string {
  const av = a ?? ''
  let out = ''
  let i = 0
  for (;;) {
    const da = i < av.length ? DIGITS.indexOf(av[i]) : 0
    const db = b == null ? 36 : i < b.length ? DIGITS.indexOf(b[i]) : 0
    if (da === db) {
      out += DIGITS[da]
      i++
      continue
    }
    if (db - da > 1) return out + DIGITS[(da + db) >> 1]
    out += DIGITS[da]
    i++
    for (;;) {
      const d = i < av.length ? DIGITS.indexOf(av[i]) : 0
      if (36 - d > 1) return out + DIGITS[(d + 36) >> 1]
      out += DIGITS[d]
      i++
    }
  }
}

/* ---------- doc view ---------- */

function DocView({
  docId,
  onOpenDoc,
  docs,
}: {
  docId: string
  onOpenDoc: (id: string) => void
  docs: Doc[]
}) {
  const [tree, setTree] = useState<DocTree | null>(null)
  const [backlinks, setBacklinks] = useState<SearchHit[]>([])
  const [panel, setPanel] = useState<'none' | 'history' | 'comments'>('none')
  const [selBlock, setSelBlock] = useState<string | null>(null)
  const [selRect, setSelRect] = useState<{ x: number; y: number } | null>(null)
  const [commentTarget, setCommentTarget] = useState<string | null>(null)

  // anchor the comment bubble just above the selected text
  const onSelectionBlock = useCallback((blockId: string | null) => {
    setSelBlock(blockId)
    if (!blockId) {
      setSelRect(null)
      return
    }
    requestAnimationFrame(() => {
      const sel = window.getSelection()
      if (!sel || sel.rangeCount === 0 || sel.isCollapsed) {
        setSelRect(null)
        return
      }
      const r = sel.getRangeAt(0).getBoundingClientRect()
      setSelRect({ x: r.left + r.width / 2, y: r.top })
    })
  }, [])

  const loadTree = useCallback(() => {
    api<DocTree>(`/api/doc/${docId}`).then(setTree).catch(console.error)
    api<SearchHit[]>(`/api/doc/${docId}/backlinks`).then(setBacklinks).catch(() => setBacklinks([]))
  }, [docId])

  useEffect(() => {
    setTree(null)
    setPanel('none')
    setCommentTarget(null)
    loadTree()
  }, [loadTree])

  const byTitle = useMemo(() => new Map(docs.map((d) => [d.title, d.id])), [docs])

  // wikilink click-through: ⌘-click anywhere in the editor text
  const onStageClick = useCallback(
    (e: React.MouseEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return
      const sel = window.getSelection()
      const text = sel?.anchorNode?.textContent ?? ''
      const offset = sel?.anchorOffset ?? 0
      const open = text.lastIndexOf('[[', offset)
      const close = text.indexOf(']]', offset)
      if (open === -1 || close === -1) return
      const target = text.slice(open + 2, close).split(/[|#]/)[0].trim()
      const name = target.split('/').pop() ?? target
      const id = byTitle.get(name)
      if (id) onOpenDoc(id)
    },
    [byTitle, onOpenDoc],
  )

  const { editable, comments, allBlocks, canvases } = useMemo(() => {
    if (!tree)
      return {
        editable: null,
        comments: [] as Block[],
        allBlocks: [] as Block[],
        canvases: [] as Block[],
      }
    const blocks: Block[] = []
    const comments: Block[] = []
    const allBlocks: Block[] = []
    const canvases: Block[] = []
    const walk = (nodes: BlockNode[]) => {
      for (const n of nodes) {
        allBlocks.push(n.block)
        if (n.block.block_type === 'comment') comments.push(n.block)
        else if (n.block.block_type === 'canvas_scene') canvases.push(n.block)
        else if (!n.block.content.startsWith('---')) blocks.push(n.block)
        walk(n.children)
      }
    }
    walk(tree.roots)
    return {
      editable: { docId, epoch: tree.doc.current_epoch, blocks },
      comments,
      allBlocks,
      canvases,
    }
  }, [tree, docId])

  if (!tree || !editable) return <div className="empty">…</div>

  // a canvas doc IS the canvas: full-stage tldraw, its own experience
  if (canvases.length > 0 && editable.blocks.length === 0) {
    return (
      <div className="canvas-doc">
        <div className="canvas-doc-head">
          <h1>{tree.doc.title}</h1>
          <span className="meta">canvas · epoch {tree.doc.current_epoch}</span>
        </div>
        <CanvasBlock block={canvases[0]} epoch={tree.doc.current_epoch} onSaved={loadTree} full />
      </div>
    )
  }

  return (
    <article className="doc" onClick={onStageClick}>
      <div className="doc-head">
        <h1>{tree.doc.title}</h1>
        {tree.doc.review_policy && <span className="meta policy">{tree.doc.review_policy}</span>}
        <StatusChip doc={tree.doc} onChanged={loadTree} />
        <span className="head-actions">
          <button
            className={`chip ${panel === 'history' ? 'on' : ''}`}
            onClick={() => setPanel(panel === 'history' ? 'none' : 'history')}
          >
            history
          </button>
          <button
            className={`chip ${panel === 'comments' ? 'on' : ''}`}
            onClick={() => setPanel(panel === 'comments' ? 'none' : 'comments')}
          >
            comments{comments.length > 0 ? ` ${comments.length}` : ''}
          </button>
        </span>
      </div>
      <DocEditor doc={editable} onSaved={() => {}} onSelectionBlock={onSelectionBlock} />
      {selBlock && selRect && panel !== 'comments' && (
        <button
          className="sel-comment"
          style={{ left: selRect.x, top: selRect.y }}
          title="comment on selection"
          onMouseDown={(e) => {
            e.preventDefault()
            setCommentTarget(selBlock)
            setPanel('comments')
            setSelRect(null)
          }}
        >
          💬
        </button>
      )}
      {panel === 'history' && <HistoryPanel docId={docId} onClose={() => setPanel('none')} />}
      {panel === 'comments' && (
        <CommentsPanel
          comments={comments}
          allBlocks={allBlocks}
          target={commentTarget}
          setTarget={setCommentTarget}
          onPosted={loadTree}
          onClose={() => setPanel('none')}
        />
      )}
      {backlinks.length > 0 && (
        <div className="backlinks">
          <span className="meta">linked from</span>
          {backlinks.map((b) => (
            <span key={b.block.id} className="backlink" onClick={() => onOpenDoc(b.block.doc_id)}>
              {b.doc_title}
            </span>
          ))}
        </div>
      )}
    </article>
  )
}

/* ---------- doc status (5.6) ---------- */

const STATUS_CYCLE = [null, 'draft', 'in-review', 'decided', 'superseded'] as const

function StatusChip({ doc, onChanged }: { doc: Doc; onChanged: () => void }) {
  const next = () => {
    const i = STATUS_CYCLE.indexOf(doc.status as (typeof STATUS_CYCLE)[number])
    const nextStatus = STATUS_CYCLE[(i + 1) % STATUS_CYCLE.length]
    api(`/api/doc/${doc.id}/status`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ status: nextStatus }),
    })
      .then(onChanged)
      .catch((e) => alert(String(e)))
  }
  return (
    <button className={`chip status-${doc.status ?? 'none'}`} onClick={next} title="cycle status">
      {doc.status ?? 'no status'}
    </button>
  )
}

/* ---------- provenance & comments panels (5.4 / 5.5) ---------- */

function opSnippet(kind: Record<string, unknown> & { op: string }): string {
  const c = typeof kind.content === 'string' ? (kind.content as string) : ''
  return c ? c.split('\n')[0].slice(0, 90) : `${kind.op} ${String(kind.target ?? '').slice(0, 8)}`
}

function HistoryPanel({ docId, onClose }: { docId: string; onClose: () => void }) {
  const [rows, setRows] = useState<HistoryRow[]>([])
  useEffect(() => {
    api<HistoryRow[]>(`/api/doc/${docId}/history`).then(setRows).catch(console.error)
  }, [docId])

  // one entry per epoch (a save/run), ops grouped beneath
  const epochs = useMemo(() => {
    const m = new Map<number, HistoryRow[]>()
    for (const r of rows) {
      const e = r.op.epoch_applied ?? -1
      if (!m.has(e)) m.set(e, [])
      m.get(e)!.push(r)
    }
    return [...m.entries()].sort((a, b) => b[0] - a[0])
  }, [rows])

  return (
    <aside className="panel">
      <div className="panel-head">
        <span>history</span>
        <button onClick={onClose}>esc</button>
      </div>
      {epochs.map(([epoch, ops]) => (
        <div key={epoch} className="epoch-group">
          <div className="epoch-line">
            <span className="meta">epoch {epoch}</span>
            <span className={`who ${ops[0].principal_kind}`}>{ops[0].principal_name}</span>
            {ops[0].op.verdict && ops[0].op.verdict !== 'green' && (
              <span className={`verdict v-${ops[0].op.verdict}`}>{ops[0].op.verdict}</span>
            )}
          </div>
          {ops.map((r) => (
            <div key={r.op.id} className="op-line">
              <span className="op-type">{r.op.kind.op}</span>
              <span className="op-snippet">{opSnippet(r.op.kind)}</span>
            </div>
          ))}
          {ops[0].op.source_refs.length > 0 && (
            <div className="refs">{ops[0].op.source_refs.join(' · ')}</div>
          )}
        </div>
      ))}
      {rows.length === 0 && <div className="palette-empty">no history</div>}
    </aside>
  )
}

function CommentsPanel({
  comments,
  allBlocks,
  target,
  setTarget,
  onPosted,
  onClose,
}: {
  comments: Block[]
  allBlocks: Block[]
  target: string | null
  setTarget: (t: string | null) => void
  onPosted: () => void
  onClose: () => void
}) {
  const [text, setText] = useState('')
  const [replyTo, setReplyTo] = useState<Block | null>(null)
  const byId = useMemo(() => new Map(allBlocks.map((b) => [b.id, b])), [allBlocks])

  const roots = comments.filter((c) => !c.parent_id || !byId.get(c.parent_id)?.refers_to)
  const repliesOf = (id: string) => comments.filter((c) => c.parent_id === id)

  const post = async () => {
    const blockId = replyTo ? replyTo.refers_to : target
    if (!text.trim() || !blockId) return
    await api('/api/comment', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ block_id: blockId, text: text.trim(), reply_to: replyTo?.id ?? null }),
    }).catch((e) => alert(String(e)))
    setText('')
    setReplyTo(null)
    setTarget(null)
    onPosted()
  }

  const snippet = (id: string | null) =>
    id ? (byId.get(id)?.content ?? '').split('\n')[0].slice(0, 70) : ''

  return (
    <aside className="panel">
      <div className="panel-head">
        <span>comments</span>
        <button onClick={onClose}>esc</button>
      </div>
      {roots.map((c) => (
        <div key={c.id} className="thread">
          <div className="thread-target">{snippet(c.refers_to)}</div>
          <CommentRow c={c} />
          {repliesOf(c.id).map((r) => (
            <div key={r.id} className="reply">
              <CommentRow c={r} />
            </div>
          ))}
          <button className="chip" onClick={() => setReplyTo(c)}>
            reply
          </button>
        </div>
      ))}
      {roots.length === 0 && !target && (
        <div className="palette-empty">no comments — select text to start a thread</div>
      )}
      {(target || replyTo) && (
        <div className="composer">
          <div className="meta">
            {replyTo ? `replying in thread` : `on: ${snippet(target)}`}
          </div>
          <textarea
            autoFocus
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') post()
            }}
            placeholder="⌘Enter to post"
          />
        </div>
      )}
    </aside>
  )
}

function CommentRow({ c }: { c: Block }) {
  return (
    <div className="comment">
      <span className="comment-text">{c.content}</span>
      <span className="meta">epoch {c.epoch}</span>
    </div>
  )
}

/* ---------- review queue ---------- */

interface FlagRow {
  block: Block
  doc_title: string
  author: string
  target_content: string | null
}

function ReviewQueue({
  onChange,
  onOpenDoc,
}: {
  onChange: (n: number) => void
  onOpenDoc: (id: string) => void
}) {
  const [rows, setRows] = useState<QueueRow[]>([])
  const [flags, setFlags] = useState<FlagRow[]>([])
  const [busy, setBusy] = useState<string | null>(null)

  const load = useCallback(() => {
    Promise.all([
      api<QueueRow[]>('/api/queue').catch(() => [] as QueueRow[]),
      api<FlagRow[]>('/api/flags').catch(() => [] as FlagRow[]),
    ]).then(([q, f]) => {
      setRows(q)
      setFlags(f)
      onChange(q.length + f.length)
    })
  }, [onChange])

  useEffect(load, [load])

  const resolve = async (annotationId: string, decision: 'accept' | 'decline') => {
    setBusy(annotationId)
    try {
      await api('/api/resolve', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ annotation_id: annotationId, decision }),
      })
    } catch (e) {
      alert(String(e))
    }
    setBusy(null)
    load()
  }

  const dismiss = async (commentId: string) => {
    setBusy(commentId)
    await api('/api/flags/dismiss', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ comment_id: commentId }),
    }).catch((e) => alert(String(e)))
    setBusy(null)
    load()
  }

  const bulk = async (ids: string[], decision: 'accept' | 'decline') => {
    if (ids.length === 0) return
    setBusy('bulk')
    await api('/api/resolve_bulk', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ annotation_ids: ids, decision }),
    }).catch((e) => alert(String(e)))
    setBusy(null)
    load()
  }

  // group proposals by proposer for bulk actions
  const byProposer = new Map<string, QueueRow[]>()
  for (const r of rows) {
    if (!byProposer.has(r.proposer)) byProposer.set(r.proposer, [])
    byProposer.get(r.proposer)!.push(r)
  }

  if (rows.length === 0 && flags.length === 0)
    return <div className="empty">review queue is empty ✓</div>

  return (
    <div className="queue">
      <h1 className="queue-title">review</h1>
      {rows.length > 1 &&
        [...byProposer.entries()].map(([who, group]) => (
          <div key={who} className="bulk-bar">
            <span className="who agent">{who}</span>
            <span className="meta">{group.length} proposals</span>
            <span className="gardener-actions">
              <button
                className="accept"
                disabled={busy !== null}
                onClick={() => bulk(group.map((r) => r.item.annotation.id), 'accept')}
              >
                accept all
              </button>
              <button
                className="bulk-decline"
                disabled={busy !== null}
                onClick={() => bulk(group.map((r) => r.item.annotation.id), 'decline')}
              >
                decline all
              </button>
            </span>
          </div>
        ))}
      {rows.map((r) => {
        const op = r.item.op
        const proposed =
          typeof op.kind.content === 'string' ? (op.kind.content as string) : JSON.stringify(op.kind)
        const parked = r.item.annotation.kind === 'parked'
        return (
          <div key={r.item.annotation.id} className={`card ${parked ? 'red' : 'yellow'}`}>
            <div className="card-head">
              <span className={`verdict ${parked ? 'v-red' : 'v-yellow'}`}>
                {parked ? 'parked' : 'applied'}
              </span>
              <span className="card-doc" onClick={() => onOpenDoc(r.item.annotation.doc_id)}>
                {r.doc_title}
              </span>
              <span className="card-meta">
                {op.kind.op} · {r.proposer}
                {op.confidence != null && ` · ${op.confidence.toFixed(2)}`}
              </span>
            </div>
            <div className="diff">
              {op.prior && (
                <div className="diff-col">
                  <div className="diff-label">{parked ? 'current' : 'before'}</div>
                  <pre>
                    {(parked ? r.current_content ?? op.prior.content : op.prior.content).slice(0, 800)}
                  </pre>
                </div>
              )}
              <div className="diff-col">
                <div className="diff-label">{parked ? 'proposed' : 'now'}</div>
                <pre>{proposed.slice(0, 800)}</pre>
              </div>
            </div>
            {op.source_refs.length > 0 && <div className="refs">{op.source_refs.join(' · ')}</div>}
            <div className="actions">
              <button
                className="accept"
                disabled={busy === r.item.annotation.id}
                onClick={() => resolve(r.item.annotation.id, 'accept')}
              >
                {parked ? 'apply' : 'keep'}
              </button>
              <button
                className="decline"
                disabled={busy === r.item.annotation.id}
                onClick={() => resolve(r.item.annotation.id, 'decline')}
              >
                {parked ? 'discard' : 'revert'}
              </button>
            </div>
          </div>
        )
      })}
      {flags.length > 0 && (
        <>
          <h2 className="runs-title">audit flags</h2>
          {flags.map((f) => (
            <div key={f.block.id} className="card flag">
              <div className="card-head">
                <span className="who agent">{f.author}</span>
                <span className="card-doc" onClick={() => onOpenDoc(f.block.doc_id)}>
                  {f.doc_title}
                </span>
              </div>
              {f.target_content && (
                <div className="thread-target">{f.target_content.split('\n')[0].slice(0, 110)}</div>
              )}
              <div className="flag-text">{f.block.content}</div>
              <div className="actions">
                <button
                  className="decline"
                  disabled={busy === f.block.id}
                  onClick={() => dismiss(f.block.id)}
                >
                  dismiss
                </button>
                <button className="chip" onClick={() => onOpenDoc(f.block.doc_id)}>
                  open doc
                </button>
              </div>
            </div>
          ))}
        </>
      )}
    </div>
  )
}
