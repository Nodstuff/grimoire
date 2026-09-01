import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import DocEditor from './editor/DocEditor'
import HotEditor, { HotDoc } from './editor/HotEditor'
import GraphView from './GraphView'
import TendPanel from './TendPanel'
import Gardeners from './Gardeners'
import CanvasBlock from './CanvasBlock'
import Sharing from './Sharing'
import SharePanel from './SharePanel'
import { notify, Notices } from './Notice'
import {
  api,
  Block,
  Doc,
  DocFederation,
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
  | { kind: 'sharing' }
  | { kind: 'home' }
type Palette = null | 'commands' | 'search' | 'newdoc' | 'newcanvas'

export default function App() {
  const [view, setViewRaw] = useState<View>({ kind: 'home' })
  // a clicked grimoire://join/… link arrives from the shell as ?join=<payload>
  const [joinPrefill, setJoinPrefill] = useState<string | null>(() => {
    const payload = new URLSearchParams(location.search).get('join')
    return payload ? `grimoire://join/${payload}` : null
  })
  const [anchor, setAnchor] = useState<string | null>(null)

  // ⌘[ / ⌘] history over views, browser-style
  const history = useRef<View[]>([{ kind: 'home' }])
  const historyIdx = useRef(0)
  const setView = useCallback((v: View) => {
    history.current = history.current.slice(0, historyIdx.current + 1)
    history.current.push(v)
    historyIdx.current = history.current.length - 1
    setViewRaw(v)
  }, [])
  const goBack = useCallback(() => {
    if (historyIdx.current > 0) {
      historyIdx.current -= 1
      setAnchor(null)
      setViewRaw(history.current[historyIdx.current])
    }
  }, [])
  const goForward = useCallback(() => {
    if (historyIdx.current < history.current.length - 1) {
      historyIdx.current += 1
      setAnchor(null)
      setViewRaw(history.current[historyIdx.current])
    }
  }, [])
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
    if (joinPrefill) {
      setViewRaw({ kind: 'sharing' })
      window.history.replaceState(null, '', '/') // don't re-trigger on reload
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshQueue])

  // live data: poll the store's change stamp; when it moves, every mounted
  // view refreshes (dataVersion threads down). Cheap — one local SQLite read.
  const [dataVersion, setDataVersion] = useState(0)
  useEffect(() => {
    let stamp: number | null = null
    let build: number | null = null
    const t = setInterval(async () => {
      try {
        const r = await api<{ stamp: number }>('/api/stamp')
        if (stamp === null) stamp = r.stamp
        else if (r.stamp !== stamp) {
          stamp = r.stamp
          setDataVersion((v) => v + 1)
          api<Doc[]>('/api/docs').then(setDocs).catch(() => {})
          refreshQueue()
        }
        // deploy landed → reload the bundle (deferred while an editor is dirty)
        const b = await api<{ build: number }>('/api/buildinfo')
        if (build === null) build = b.build
        else if (b.build !== build && !document.querySelector('.save-state.dirty, .save-state.saving')) {
          location.reload()
        }
      } catch {
        // daemon restarting mid-deploy — next tick catches up
      }
    }, 2500)
    return () => clearInterval(t)
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
      } else if (mod && e.key === '[') {
        e.preventDefault()
        goBack()
      } else if (mod && e.key === ']') {
        e.preventDefault()
        goForward()
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
  }, [goBack, goForward, setView])

  const openDoc = useCallback(
    (id: string, toAnchor?: string) => {
      setAnchor(toAnchor ?? null)
      setView({ kind: 'doc', id })
      setPalette(null)
    },
    [setView],
  )

  // canvas nodes fire wikilink clicks as events (CanvasBlock has no doc list)
  useEffect(() => {
    const onOpen = (e: Event) => {
      const title = (e as CustomEvent<string>).detail
      const target = docs.find((d) => d.title === title)
      if (target) openDoc(target.id)
    }
    window.addEventListener('grimoire:open-doc', onOpen)
    return () => window.removeEventListener('grimoire:open-doc', onOpen)
  }, [docs, openDoc])

  return (
    <div className="app">
      <Notices />
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
        {view.kind === 'doc' && (
          <DocView
            docId={view.id}
            onOpenDoc={openDoc}
            docs={docs}
            dataVersion={dataVersion}
            anchor={anchor}
          />
        )}
        {view.kind === 'review' && (
          <ReviewQueue
            onChange={setQueueCount}
            onOpenDoc={openDoc}
            dataVersion={dataVersion}
          />
        )}
        {view.kind === 'runs' && <Gardeners dataVersion={dataVersion} />}
        {view.kind === 'sharing' && (
          <Sharing
            docs={docs}
            dataVersion={dataVersion}
            onOpenDoc={openDoc}
            prefillLink={joinPrefill}
            onPrefillConsumed={() => setJoinPrefill(null)}
          />
        )}
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
            if (a === 'sharing') setView({ kind: 'sharing' })
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
  onAction: (a: 'review' | 'runs' | 'tree' | 'home' | 'newdoc' | 'newcanvas' | 'graph' | 'sharing') => void
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
    { label: 'Sharing & contacts', run: () => onAction('sharing') },
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
          if (e.key === 'Enter') create().catch((err) => notify(String(err)))
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

  const [pendingMove, setPendingMove] = useState<{
    dragged: string
    parent: string | null
    sortKey: string | null
    sharedRoot: string
  } | null>(null)

  const byId = useMemo(() => new Map(docs.map((d) => [d.id, d])), [docs])
  // the nearest shared ancestor (incl. self) of a doc, if any
  const sharedRootOf = (id: string | null): Doc | null => {
    let cur = id ? byId.get(id) : undefined
    while (cur) {
      if (cur.is_shared) return cur
      cur = cur.parent_id ? byId.get(cur.parent_id) : undefined
    }
    return null
  }

  // the actual move; the server may refuse (e.g. a mirror into a shared
  // subtree) — its error string surfaces as a notice
  const commitMove = async (dragged: string, parent: string | null, sortKey: string | null) => {
    try {
      await api(`/api/doc/${dragged}/move`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: parent, sort_key: sortKey }),
      })
      onChanged()
    } catch (e) {
      notify(String(e))
    }
  }

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
    // moving INTO a shared subtree makes the doc visible to its grantees on
    // their next pull — loud, explicit confirm (ADR 0002 edge semantics)
    const wasShared = sharedRootOf(byId.get(dragged)?.parent_id ?? null)
    const nowShared = sharedRootOf(parent)
    if (nowShared && nowShared.id !== wasShared?.id) {
      setPendingMove({ dragged, parent, sortKey, sharedRoot: nowShared.title })
      return
    }
    await commitMove(dragged, parent, sortKey)
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
            {d.is_tended && <span className="tend-dot" title="tended by agents" />}
            {d.mirror_permission && (
              <span className="mirror-badge" title={`shared with you (${d.mirror_permission})`}>⇄</span>
            )}
            {d.is_shared && <span className="shared-badge" title="you share this subtree">↗</span>}
            {!d.mirror_permission && (
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
            )}
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
      {pendingMove && (
        <div className="move-confirm">
          <div>
            “{byId.get(pendingMove.dragged)?.title ?? '?'}” will become visible to
            everyone “{pendingMove.sharedRoot}” is shared with
          </div>
          <div className="move-confirm-actions">
            <button
              className="accept"
              onClick={() => {
                const m = pendingMove
                setPendingMove(null)
                commitMove(m.dragged, m.parent, m.sortKey)
              }}
            >
              share it
            </button>
            <button className="decline" onClick={() => setPendingMove(null)}>
              cancel
            </button>
          </div>
        </div>
      )}
      <div
        className="tree-root"
        onDragOver={(e) => {
          // dropping on empty space = move to root
          if (dragging && e.target === e.currentTarget) e.preventDefault()
        }}
        onDrop={(e) => {
          if (dragging && e.target === e.currentTarget) {
            commitMove(dragging, null, null)
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

/** Click-to-rename doc title. Inbound [[wikilinks]] resolve by title and are
 * not rewritten on rename — they dangle until edited. */
function DocTitle({ doc, onRenamed }: { doc: Doc; onRenamed: () => void }) {
  const [editing, setEditing] = useState(false)
  const [value, setValue] = useState(doc.title)

  const save = async () => {
    setEditing(false)
    const title = value.trim()
    if (!title || title === doc.title) {
      setValue(doc.title)
      return
    }
    await api(`/api/doc/${doc.id}/rename`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ title }),
    }).catch((e) => {
      console.error(e)
      setValue(doc.title)
    })
    onRenamed()
  }

  if (!editing)
    return (
      <h1 className="doc-title" title="click to rename" onClick={() => {
        setValue(doc.title)
        setEditing(true)
      }}>
        {doc.title}
      </h1>
    )
  return (
    <input
      className="doc-title-edit"
      autoFocus
      value={value}
      onChange={(e) => setValue(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === 'Enter') save()
        if (e.key === 'Escape') {
          setValue(doc.title)
          setEditing(false)
        }
      }}
      onBlur={save}
    />
  )
}

function DocView({
  docId,
  onOpenDoc,
  docs,
  dataVersion,
  anchor,
}: {
  docId: string
  onOpenDoc: (id: string, anchor?: string) => void
  docs: Doc[]
  dataVersion: number
  anchor?: string | null
}) {
  const [tree, setTree] = useState<DocTree | null>(null)
  const [backlinks, setBacklinks] = useState<SearchHit[]>([])
  const [fed, setFed] = useState<DocFederation | null>(null)
  const [hot, setHot] = useState<HotDoc | null>(null)
  const mirrorRef = useRef<unknown>(null)
  const [panel, setPanel] = useState<'none' | 'history' | 'comments' | 'tend' | 'share'>('none')
  const [selBlock, setSelBlock] = useState<string | null>(null)
  const [selRect, setSelRect] = useState<{ x: number; y: number } | null>(null)
  const [commentTarget, setCommentTarget] = useState<string | null>(null)
  // epochs this editor produced itself — external changes are anything above
  const ownEpoch = useRef(0)
  const [editorGen, setEditorGen] = useState(0)

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
    api<DocFederation>(`/api/doc/${docId}/federation`).then(setFed).catch(() => setFed(null))
  }, [docId])

  useEffect(() => {
    setTree(null)
    setPanel('none')
    setCommentTarget(null)
    setHot(null)
    ownEpoch.current = 0
    loadTree()
  }, [loadTree])

  // live refresh: when the store changed and this doc moved past what our own
  // saves produced, reload it and remount the editor with the fresh content
  // (skipped while dirty — the pending autosave lands first, next tick catches up)
  useEffect(() => {
    if (dataVersion === 0 || !tree) return
    api<DocTree>(`/api/doc/${docId}`)
      .then((fresh) => {
        const known = Math.max(tree.doc.current_epoch, ownEpoch.current)
        const dirty = document.querySelector('.save-state.dirty, .save-state.saving')
        if (fresh.doc.current_epoch > known && !dirty) {
          setTree(fresh)
          setEditorGen((g) => g + 1)
        } else if (fresh.doc.status !== tree.doc.status) {
          setTree(fresh)
        }
        api<SearchHit[]>(`/api/doc/${docId}/backlinks`).then(setBacklinks).catch(() => {})
        api<DocFederation>(`/api/doc/${docId}/federation`).then(setFed).catch(() => {})
      })
      .catch(() => {})
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dataVersion])

  const byTitle = useMemo(() => new Map(docs.map((d) => [d.title, d.id])), [docs])


  // anchor from a link: [[Doc#^block-uuid]] finds the block by stable id,
  // [[Doc#Heading]] falls back to text match. Scroll + flash.
  useEffect(() => {
    if (!anchor || !tree) return
    // retry across editor remounts (live refresh can race the first attempt)
    const attempt = () => {
      let el: Element | null = null
      if (anchor.startsWith('^')) {
        el = document.querySelector(`[data-block-id="${anchor.slice(1)}"]`)
      }
      if (!el) {
        const needle = anchor.replace(/^\^/, '').toLowerCase()
        el =
          [...document.querySelectorAll(
            '.ProseMirror h1, .ProseMirror h2, .ProseMirror h3, .ProseMirror h4, .ProseMirror p',
          )].find((n) => n.textContent?.toLowerCase().includes(needle)) ?? null
      }
      if (el) {
        el.scrollIntoView({ block: 'start' })
        el.classList.add('anchor-flash')
        setTimeout(() => el.classList.remove('anchor-flash'), 1600)
        return true
      }
      return false
    }
    const timers = [250, 900, 1800].map((ms) => setTimeout(attempt, ms))
    return () => timers.forEach(clearTimeout)
  }, [anchor, tree])

  // wikilink click-through: decorated targets carry data-target
  const onStageClick = useCallback(
    (e: React.MouseEvent) => {
      const el = (e.target as HTMLElement).closest('.wl-target') as HTMLElement | null
      if (!el) return
      const target = el.dataset.target ?? ''
      const [path, fragment] = target.split('#')
      const name = path.split('/').pop()?.trim() ?? path
      const id = byTitle.get(name)
      if (id) {
        e.preventDefault()
        onOpenDoc(id, fragment?.trim())
      }
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

  // go live: shared by the ⚡ chip, auto-join, and auto-hot escalation.
  // The seeder NEVER seeds from in-memory state — it fetches the doc fresh
  // and requires the fetched epoch to match the frozen epoch, so a save that
  // landed moments before the session started can't be lost.
  const goLive = useCallback(async () => {
    if (!editable) return
    try {
      const r = await api<{ frozen_epoch: number; seed: boolean }>(
        `/api/doc/${docId}/hot/start`,
        { method: 'POST' },
      )
      let blocks = editable.blocks
      if (r.seed) {
        for (let attempt = 0; attempt < 5; attempt++) {
          const fresh = await api<DocTree>(`/api/doc/${docId}`)
          // freshest content wins even on epoch mismatch (a save that raced
          // the freeze); the gate scores the flatten against the stale base
          // rather than anything being silently lost
          blocks = editableBlocksOf(fresh)
          if (fresh.doc.current_epoch === r.frozen_epoch) break
          await new Promise((res) => setTimeout(res, 300))
        }
      }
      setHot({
        docId,
        frozenEpoch: r.frozen_epoch,
        seed: r.seed,
        blocks,
      })
    } catch (e) {
      console.error(e)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [docId, editable])

  // a live session started elsewhere (second window, another instance,
  // recovered journal): join it rather than editing cold. And when TWO
  // editors are typing the same cold doc, escalate to a live session
  // (P2.1 auto-hot) — only from a clean editor, so no keystrokes are lost.
  useEffect(() => {
    if (!tree || hot) return
    api<{ hot: boolean; frozen_epoch?: number; editors?: number }>(
      `/api/doc/${docId}/hot/status`,
    )
      .then((st) => {
        if (!editable) return
        if (st.hot) {
          setHot({
            docId,
            frozenEpoch: st.frozen_epoch ?? tree.doc.current_epoch,
            seed: false,
            blocks: editable.blocks,
          })
        } else if ((st.editors ?? 0) >= 2) {
          const dirty = document.querySelector('.save-state.dirty, .save-state.saving')
          if (!dirty) goLive()
        }
      })
      .catch(() => {})
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tree, dataVersion])

  if (!tree || !editable) return <div className="empty">…</div>

  // a canvas doc IS the canvas: full-stage React Flow editor, its own experience
  if (canvases.length > 0 && editable.blocks.length === 0) {
    return (
      <div className="canvas-doc">
        <div className="canvas-doc-head">
          <DocTitle doc={tree.doc} onRenamed={loadTree} />
          <span className="meta">canvas · epoch {tree.doc.current_epoch}</span>
        </div>
        <CanvasBlock block={canvases[0]} epoch={tree.doc.current_epoch} onSaved={loadTree} full />
      </div>
    )
  }

  const mirror = fed?.mirror ?? null
  mirrorRef.current = mirror
  const pendingProposals = (fed?.outbound ?? []).filter((o) => o.state === 'pending')

  return (
    <article className="doc" onClick={onStageClick}>
      {mirror && (
        <div className="mirror-banner">
          ⇄ shared by <b>{mirror.owner_petname}</b>
          {mirror.permission === 'view'
            ? ' · view only'
            : ' · your edits go to them as suggestions'}
          {pendingProposals.length > 0 && (
            <span className="pending-chip">
              {pendingProposals.length} suggestion{pendingProposals.length > 1 ? 's' : ''} awaiting{' '}
              {mirror.owner_petname}
            </span>
          )}
        </div>
      )}
      <div className="doc-head">
        {mirror ? (
          <h1 className="doc-title readonly">{tree.doc.title}</h1>
        ) : (
          <DocTitle doc={tree.doc} onRenamed={loadTree} />
        )}
        {tree.doc.review_policy && <span className="meta policy">{tree.doc.review_policy}</span>}
        {!mirror && <StatusChip doc={tree.doc} onChanged={loadTree} />}
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
          {!mirror && (
            <button
              className={`chip ${panel === 'tend' ? 'on' : ''} ${docs.find((d) => d.id === docId)?.is_tended ? 'tended' : ''}`}
              onClick={() => setPanel(panel === 'tend' ? 'none' : 'tend')}
            >
              {docs.find((d) => d.id === docId)?.is_tended ? '🌿 tended' : 'tend'}
            </button>
          )}
          {!mirror && (
            <button
              className={`chip ${panel === 'share' ? 'on' : ''} ${(fed?.shares.length ?? 0) > 0 ? 'shared' : ''}`}
              onClick={() => setPanel(panel === 'share' ? 'none' : 'share')}
            >
              {(fed?.shares.length ?? 0) > 0 ? '↗ shared' : 'share'}
            </button>
          )}
          {!hot && editable && (!mirror || mirror.permission === 'propose') && (
            <button
              className="chip"
              title="start a live co-editing session"
              onClick={goLive}
            >
              ⚡ go live
            </button>
          )}
        </span>
      </div>
      {hot ? (
        <HotEditor
          key={`hot:${docId}`}
          doc={hot}
          onEnded={() => {
            setHot(null)
            setEditorGen((g) => g + 1)
            loadTree()
          }}
        />
      ) : (
      <DocEditor
        key={`${docId}:${editorGen}`}
        doc={editable}
        mode={mirror ? (mirror.permission === 'propose' ? 'propose' : 'readonly') : 'direct'}
        onSaved={(e) => {
          ownEpoch.current = Math.max(ownEpoch.current, e)
        }}
        onProposed={() => {
          // pessimistic mirror: reset the editor to the pristine mirror and
          // show the pending chip
          setEditorGen((g) => g + 1)
          loadTree()
        }}
        onSelectionBlock={onSelectionBlock}
      />
      )}
      {selBlock && selRect && panel !== 'comments' && (
        <span className="sel-actions" style={{ left: selRect.x, top: selRect.y }}>
          <button
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
          <button
            title="copy block link"
            onMouseDown={(e) => {
              e.preventDefault()
              navigator.clipboard
                .writeText(`[[${tree.doc.title}#^${selBlock}]]`)
                .catch(() => {})
              setSelRect(null)
            }}
          >
            🔗
          </button>
        </span>
      )}
      {panel === 'history' && <HistoryPanel docId={docId} onClose={() => setPanel('none')} />}
      {panel === 'share' && (
        <SharePanel
          doc={tree.doc}
          fed={fed ?? { mirror: null, shares: [], outbound: [] }}
          onChanged={loadTree}
          onClose={() => setPanel('none')}
        />
      )}
      {panel === 'tend' && (
        <TendPanel doc={tree.doc} onClose={() => setPanel('none')} dataVersion={dataVersion} />
      )}
      {panel === 'comments' && (
        <CommentsPanel
          comments={comments}
          allBlocks={allBlocks}
          target={commentTarget}
          setTarget={setCommentTarget}
          upstream={!!mirror}
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

/** The blocks the editor works on: content flow only — comments, frontmatter
 * and canvases excluded (the same walk as DocView's editable memo). */
function editableBlocksOf(tree: DocTree): Block[] {
  const blocks: Block[] = []
  const walk = (nodes: BlockNode[]) => {
    for (const n of nodes) {
      if (
        n.block.block_type !== 'comment' &&
        n.block.block_type !== 'canvas_scene' &&
        !n.block.content.startsWith('---')
      ) {
        blocks.push(n.block)
      }
      walk(n.children)
    }
  }
  walk(tree.roots)
  return blocks
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
      .catch((e) => notify(String(e)))
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
  upstream = false,
  onPosted,
  onClose,
}: {
  comments: Block[]
  allBlocks: Block[]
  target: string | null
  setTarget: (t: string | null) => void
  /** mirror docs: comments post to the owner and echo back via pull */
  upstream?: boolean
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
    // mirror docs: the comment channel — applied on the owner, echoed back
    const endpoint = upstream ? '/admin/comment_upstream' : '/api/comment'
    await api(endpoint, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ block_id: blockId, text: text.trim(), reply_to: replyTo?.id ?? null }),
    }).catch((e) => notify(String(e)))
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
  dataVersion,
}: {
  onChange: (n: number) => void
  onOpenDoc: (id: string) => void
  dataVersion: number
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

  useEffect(load, [load, dataVersion])

  const resolve = async (annotationId: string, decision: 'accept' | 'decline') => {
    setBusy(annotationId)
    try {
      await api('/api/resolve', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ annotation_id: annotationId, decision }),
      })
    } catch (e) {
      notify(String(e))
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
    }).catch((e) => notify(String(e)))
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
    }).catch((e) => notify(String(e)))
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
