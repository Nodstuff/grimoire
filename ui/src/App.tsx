import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import DocEditor from './editor/DocEditor'
import GraphView from './GraphView'
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
type Palette = null | 'commands' | 'search' | 'newdoc'

export default function App() {
  const [view, setView] = useState<View>({ kind: 'home' })
  const [docs, setDocs] = useState<Doc[]>([])
  const [treeOpen, setTreeOpen] = useState(false)
  const [palette, setPalette] = useState<Palette>(null)
  const [queueCount, setQueueCount] = useState(0)

  const refreshQueue = useCallback(() => {
    api<QueueRow[]>('/api/queue').then((q) => setQueueCount(q.length)).catch(() => {})
  }, [])

  useEffect(() => {
    api<Doc[]>('/api/docs').then(setDocs).catch(console.error)
    refreshQueue()
  }, [refreshQueue])

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
        {view.kind === 'runs' && <Runs />}
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
            setPalette(null)
          }}
          onClose={() => setPalette(null)}
        />
      )}
      {palette === 'search' && <SearchPalette onOpenDoc={openDoc} onClose={() => setPalette(null)} />}
      {palette === 'newdoc' && (
        <NewDocPalette
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
  onAction: (a: 'review' | 'runs' | 'tree' | 'home' | 'newdoc' | 'graph') => void
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
    { label: 'Gardener runs', run: () => onAction('runs') },
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
      .map((d) => ({ label: d.title, hint: 'doc', run: () => onOpenDoc(d.id) }))
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
  onCreated,
  onClose,
}: {
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
    onCreated(d.id)
  }

  return (
    <PaletteShell onClose={onClose}>
      <input
        ref={inputRef}
        placeholder="New doc title…"
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

function DocTreeNav({
  docs,
  selected,
  onSelect,
  onClose,
}: {
  docs: Doc[]
  selected: string | null
  onSelect: (id: string) => void
  onClose: () => void
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

  const renderLevel = (parent: string | null, depth: number): React.ReactNode =>
    (childrenOf.get(parent) ?? []).map((d) => {
      const isDir = !!childrenOf.get(d.id)?.length
      const isOpen = openDirs.has(d.id)
      return (
        <div key={d.id}>
          <div
            className={`tree-item ${selected === d.id ? 'sel' : ''}`}
            style={{ paddingLeft: 10 + depth * 14 }}
            onClick={() => {
              if (isDir)
                setOpenDirs((s) => {
                  const n = new Set(s)
                  n.has(d.id) ? n.delete(d.id) : n.add(d.id)
                  return n
                })
              else onSelect(d.id)
            }}
          >
            <span className="tree-icon">{isDir ? (isOpen ? '▾' : '▸') : ''}</span>
            {d.title}
          </div>
          {isDir && isOpen && renderLevel(d.id, depth + 1)}
        </div>
      )
    })

  return (
    <aside className="sidebar">
      <div className="sidebar-head">
        <span>files</span>
        <button onClick={onClose}>⌘T</button>
      </div>
      {renderLevel(null, 0)}
    </aside>
  )
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

  const { editable, comments, allBlocks } = useMemo(() => {
    if (!tree) return { editable: null, comments: [] as Block[], allBlocks: [] as Block[] }
    const blocks: Block[] = []
    const comments: Block[] = []
    const allBlocks: Block[] = []
    const walk = (nodes: BlockNode[]) => {
      for (const n of nodes) {
        allBlocks.push(n.block)
        if (n.block.block_type === 'comment') comments.push(n.block)
        else if (!n.block.content.startsWith('---')) blocks.push(n.block)
        walk(n.children)
      }
    }
    walk(tree.roots)
    return { editable: { docId, epoch: tree.doc.current_epoch, blocks }, comments, allBlocks }
  }, [tree, docId])

  if (!tree || !editable) return <div className="empty">…</div>

  return (
    <article className="doc" onClick={onStageClick}>
      <div className="doc-head">
        <h1>{tree.doc.title}</h1>
        {tree.doc.review_policy && <span className="meta policy">{tree.doc.review_policy}</span>}
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

function ReviewQueue({
  onChange,
  onOpenDoc,
}: {
  onChange: (n: number) => void
  onOpenDoc: (id: string) => void
}) {
  const [rows, setRows] = useState<QueueRow[]>([])
  const [busy, setBusy] = useState<string | null>(null)

  const load = useCallback(() => {
    api<QueueRow[]>('/api/queue')
      .then((q) => {
        setRows(q)
        onChange(q.length)
      })
      .catch(console.error)
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

  if (rows.length === 0) return <div className="empty">review queue is empty ✓</div>

  return (
    <div className="queue">
      <h1 className="queue-title">review</h1>
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
    </div>
  )
}

/* ---------- runs ---------- */

function Runs() {
  const [runs, setRuns] = useState<GardenerRun[]>([])
  useEffect(() => {
    api<GardenerRun[]>('/api/runs').then(setRuns).catch(console.error)
  }, [])
  if (runs.length === 0) return <div className="empty">no gardener runs yet</div>
  return (
    <div className="runs">
      <h1 className="queue-title">gardeners</h1>
      {runs.map((r) => (
        <div key={r.id} className="run">
          <div className="run-head">
            <span className={`status ${r.status}`}>{r.status}</span>
            {r.tokens_used != null && <span className="meta">{r.tokens_used} tokens</span>}
          </div>
          <pre>{r.summary}</pre>
        </div>
      ))}
    </div>
  )
}
