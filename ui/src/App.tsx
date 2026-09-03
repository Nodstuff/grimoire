import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import DocEditor from './editor/DocEditor'
import HotEditor, { AgentStatus, HotDoc } from './editor/HotEditor'
import TendPanel from './TendPanel'
import Gardeners from './Gardeners'
import Sharing from './Sharing'
import SharePanel from './SharePanel'
import PaletteShell from './PaletteShell'
import Profile, { FirstRunName, loadProfile } from './Profile'
import Trash, { restoreDoc } from './Trash'
import ImportFolder from './ImportFolder'
import ReviewRail from './ReviewRail'
import { notify, errText, Notices } from './Notice'

// heavy views load on first use: xyflow + html-to-image (canvas) and
// force-graph (graph) are not part of the boot bundle
const CanvasBlock = lazy(() => import('./CanvasBlock'))
const GraphView = lazy(() => import('./GraphView'))
const Loading = () => <div className="lazy-loading">loading…</div>
import { resolveShortcut } from './shortcuts'
import { parseDeepLink, scrubDeepLink } from './deeplink'
import { buildHighlightMap, targetBlockOf } from './review'
import { activityLine, loadLastSeen, storeLastSeen, unseenActivity } from './activity'
import { advanceEvents, EventsCursor, EventsResponse, INITIAL_CURSOR, liveEventLine } from './live'
import { chipText } from './shares'
import { compareSortKey, keyForPosition } from './editor/diff'
import {
  api,
  ApiError,
  ActivityItem,
  Block,
  Doc,
  DocFederation,
  DocTree,
  BlockNode,
  HistoryRow,
  QueueRow,
  SearchHit,
  GardenerRun,
  Profile as ProfileRow,
  ShareOffer,
} from './types'

type View =
  | { kind: 'doc'; id: string }
  | { kind: 'review' }
  | { kind: 'runs' }
  | { kind: 'graph' }
  | { kind: 'sharing' }
  | { kind: 'profile' }
  | { kind: 'trash' }
  | { kind: 'home' }
type Palette = null | 'commands' | 'open' | 'search' | 'newdoc' | 'newcanvas' | 'help' | 'ask'

/** How a doc is opened: `anchor` is a [[Doc#fragment]] target (`^uuid` for a
 * block); `review` opens the in-editor review rail; `blockId` scrolls to that
 * block (sugar for anchor `^blockId`). */
export interface OpenDocOpts {
  anchor?: string
  review?: boolean
  blockId?: string
}
export type OpenDoc = (id: string, opts?: string | OpenDocOpts) => void

export default function App() {
  const [view, setViewRaw] = useState<View>({ kind: 'home' })
  // a clicked grimoire://join/… link arrives from the shell as ?join=<payload>
  const [joinPrefill, setJoinPrefill] = useState<string | null>(() => {
    const payload = new URLSearchParams(location.search).get('join')
    return payload ? `grimoire://join/${payload}` : null
  })
  const [anchor, setAnchor] = useState<string | null>(null)
  // opened FROM the review queue (or with { review: true }): DocView opens its rail
  const [reviewIntent, setReviewIntent] = useState(false)

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
      setReviewIntent(false)
      setViewRaw(history.current[historyIdx.current])
    }
  }, [])
  const goForward = useCallback(() => {
    if (historyIdx.current < history.current.length - 1) {
      historyIdx.current += 1
      setAnchor(null)
      setReviewIntent(false)
      setViewRaw(history.current[historyIdx.current])
    }
  }, [])
  const [docs, setDocs] = useState<Doc[]>([])
  const [treeOpen, setTreeOpen] = useState(false)
  const [palette, setPalette] = useState<Palette>(null)
  const [queueCount, setQueueCount] = useState(0)
  // invites v2: open share requests ride the same header chip
  const [offerCount, setOfferCount] = useState(0)
  // first-run name prompt: shown until the install-default name is confirmed.
  // null = no profile route (older daemon) or not loaded yet → no prompt.
  const [profile, setProfile] = useState<ProfileRow | null>(null)
  useEffect(() => {
    loadProfile().then(setProfile)
  }, [])

  const refreshQueue = useCallback(() => {
    Promise.all([
      api<QueueRow[]>('/api/queue').then((q) => q.length).catch(() => 0),
      api<{ block: { id: string } }[]>('/api/flags').then((f) => f.length).catch(() => 0),
    ]).then(([q, f]) => setQueueCount(q + f))
    api<ShareOffer[]>('/admin/offers')
      .then((o) => setOfferCount(Array.isArray(o) ? o.length : 0))
      .catch(() => setOfferCount(0))
  }, [])

  // ?doc=<uuid>[&block=<uuid>][&tab=<name>]: the shell or an embedding host
  // opens the page ON a doc or view. Read once, scrubbed off the URL like
  // admin_token so a reload lands on home, not back on the doc.
  const deepLink = useRef(parseDeepLink(location.search))

  useEffect(() => {
    const link = deepLink.current
    api<Doc[]>('/api/docs')
      .then((list) => {
        setDocs(list)
        if (!link?.doc) return
        if (list.some((d) => d.id === link.doc)) openDocRef.current(link.doc, { blockId: link.block })
        else notify('That doc is not here — it may have been deleted or moved to the Trash')
      })
      .catch(console.error)
    refreshQueue()
    if (link) {
      if (link.tab && link.tab !== 'home') setViewRaw({ kind: link.tab })
      window.history.replaceState(null, '', location.pathname + scrubDeepLink(location.search) + location.hash)
    }
    if (joinPrefill) {
      setViewRaw({ kind: 'sharing' })
      window.history.replaceState(null, '', '/') // don't re-trigger on reload
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshQueue])

  // live data: poll the store's change stamp; when it moves, every mounted
  // view refreshes (dataVersion threads down). Cheap — one local SQLite read.
  const [dataVersion, setDataVersion] = useState(0)
  // the stamp poll doubles as the liveness check: three misses in a row
  // (7.5s) = the background service is down; one hit clears it
  const [daemonDown, setDaemonDown] = useState(false)
  // owner→grantee nudges (GET /api/events): a doc_changed for the doc that is
  // open makes DocView reload at once instead of waiting for the next pull
  const [liveChange, setLiveChange] = useState<{ docId: string; n: number } | null>(null)
  const openDocRef = useRef<OpenDoc>(() => {})
  useEffect(() => {
    let stamp: number | null = null
    let build: number | null = null
    let cursor: EventsCursor = INITIAL_CURSOR
    const pollEvents = async () => {
      // older daemon without the route: api() throws, cursor stays put
      const resp = await api<EventsResponse>(`/api/events?since=${cursor.since}`).catch(() => null)
      const r = advanceEvents(cursor, resp)
      cursor = r.cursor
      for (const ev of r.fresh) {
        const line = liveEventLine(ev)
        if (ev.kind === 'share_offered') {
          // durable request: the toast just points at the Shares page
          if (line) notify(line, 'ok', { ttlMs: 15_000, onClick: () => setViewRaw({ kind: 'sharing' }) })
          refreshQueue()
        } else if (line) {
          notify(line, 'ok', { onClick: () => openDocRef.current(ev.doc_id) })
        } else if (ev.kind === 'doc_changed' && ev.doc_id) {
          setLiveChange((c) => ({ docId: ev.doc_id, n: (c?.n ?? 0) + 1 }))
        }
      }
    }
    // don't wait a whole tick for the first nudge check
    pollEvents().catch(() => {})
    let misses = 0
    let inFlight = false
    const t = setInterval(async () => {
      // a slow daemon must not stack ticks (each one is up to three requests)
      if (inFlight) return
      inFlight = true
      try {
        const r = await api<{ stamp: number; build?: number; version?: string }>('/api/stamp')
        misses = 0
        setDaemonDown(false)
        if (stamp === null) stamp = r.stamp
        else if (r.stamp !== stamp) {
          stamp = r.stamp
          setDataVersion((v) => v + 1)
          api<Doc[]>('/api/docs').then(setDocs).catch(() => {})
          refreshQueue()
        }
        await pollEvents()
        // deploy landed → reload the bundle (deferred while an editor is dirty).
        // Newer daemons carry the build on the stamp; fall back to the
        // dedicated route only when it is absent.
        const b = typeof r.build === 'number' ? r.build : (await api<{ build: number }>('/api/buildinfo')).build
        if (build === null) build = b
        else if (b !== build && !document.querySelector('.save-state.dirty, .save-state.saving')) {
          location.reload()
        }
      } catch {
        // restarting mid-deploy, or actually down — say so after 3 misses
        misses += 1
        if (misses >= 3) setDaemonDown(true)
      } finally {
        inFlight = false
      }
    }, 2500)
    return () => clearInterval(t)
  }, [refreshQueue])

  // owner notifications: maintainer-tier (green) edits land directly, so the
  // activity feed is the only signal. Poll on data changes; toast each
  // unseen item once; remember the newest seen op across launches.
  const lastSeenOp = useRef<string | null>(loadLastSeen())
  useEffect(() => {
    api<ActivityItem[]>('/api/activity?limit=20')
      .then((items) => {
        if (!Array.isArray(items) || items.length === 0) return
        for (const it of unseenActivity(items, lastSeenOp.current).reverse()) {
          notify(activityLine(it), 'ok')
        }
        lastSeenOp.current = items[0].op_id
        storeLastSeen(items[0].op_id)
      })
      .catch(() => {})
  }, [dataVersion])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const action = resolveShortcut(e)
      if (!action) return
      // Esc isn't a combo — leave its default (blur inputs etc.) intact.
      if (action === 'escape') {
        setPalette(null)
        return
      }
      // `?` has no modifier: ignore it while typing so it still inserts a `?`.
      if (action === 'help') {
        const el = e.target as HTMLElement | null
        const typing =
          !!el &&
          (el.tagName === 'INPUT' ||
            el.tagName === 'TEXTAREA' ||
            el.isContentEditable)
        if (typing) return
        e.preventDefault()
        setPalette((p) => (p === 'help' ? null : 'help'))
        return
      }
      // every mod combo: ⌘P is webview Print, ⌘O the open-file dialog, etc.
      e.preventDefault()
      switch (action) {
        case 'commands':
          setPalette((p) => (p === 'commands' ? null : 'commands'))
          break
        case 'open':
          setPalette((p) => (p === 'open' ? null : 'open'))
          break
        case 'search':
          // ⌘S while writing means "save" to every editor user: the doc
          // autosaves, so just say so instead of opening search
          if (e.key.toLowerCase() === 's' && (e.target as HTMLElement | null)?.closest('.ProseMirror')) {
            notify('autosaved', 'ok', { ttlMs: 1500 })
            break
          }
          setPalette((p) => (p === 'search' ? null : 'search'))
          break
        case 'ask':
          setPalette((p) => (p === 'ask' ? null : 'ask'))
          break
        case 'tree':
          setTreeOpen((t) => !t)
          break
        case 'newdoc':
          setPalette((p) => (p === 'newdoc' ? null : 'newdoc'))
          break
        case 'newcanvas':
          setPalette((p) => (p === 'newcanvas' ? null : 'newcanvas'))
          break
        case 'review':
          setView({ kind: 'review' })
          break
        case 'gardeners':
          setView({ kind: 'runs' })
          break
        case 'reload':
          location.reload()
          break
        case 'back':
          goBack()
          break
        case 'forward':
          goForward()
          break
        case 'home':
          setView({ kind: 'home' })
          break
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [goBack, goForward, setView])

  const openDoc = useCallback<OpenDoc>(
    (id, opts) => {
      const o: OpenDocOpts = typeof opts === 'string' ? { anchor: opts } : (opts ?? {})
      setAnchor(o.anchor ?? (o.blockId ? `^${o.blockId}` : null))
      setReviewIntent(!!o.review)
      setView({ kind: 'doc', id })
      setPalette(null)
    },
    [setView],
  )
  openDocRef.current = openDoc

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
      {daemonDown && (
        <div className="daemon-banner" role="status">
          Grimoire’s background service is not responding — edits are kept in the editor and will save when it is back
        </div>
      )}
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
            {docs.length === 0 ? (
              <div className="home-start">
                <div className="home-start-title">Welcome to Grimoire</div>
                <div><kbd>⌘N</kbd> create your first doc</div>
                <div><kbd>⌘K</kbd> → Shares &amp; contacts to join a share someone sent you</div>
                <div>
                  <ImportFolder
                    label="Already have notes? Import a folder of Markdown…"
                    onDone={() => api<Doc[]>('/api/docs').then(setDocs).catch(() => {})}
                  />
                </div>
                <div><kbd>?</kbd> all shortcuts</div>
              </div>
            ) : (
            <div className="home-hints">
              <span><kbd>⌘K</kbd> commands</span>
              <span><kbd>⌘O</kbd> open</span>
              <span><kbd>⌘P</kbd> search</span>
              <span><kbd>⌘T</kbd> tree</span>
              <span><kbd>⌘N</kbd> new doc</span>
              <span><kbd>⌘⇧N</kbd> canvas</span>
              <span><kbd>⌘⇧R</kbd> review</span>
              <span><kbd>⌘G</kbd> gardeners</span>
              <span><kbd>⌘W</kbd> home</span>
              <span><kbd>⌘[</kbd> <kbd>⌘]</kbd> history</span>
              <span><kbd>?</kbd> all shortcuts</span>
            </div>
            )}
          </div>
        )}
        {view.kind === 'doc' && (
          <DocView
            key={view.id}
            docId={view.id}
            onOpenDoc={openDoc}
            docs={docs}
            dataVersion={dataVersion}
            liveChange={liveChange}
            anchor={anchor}
            reviewIntent={reviewIntent}
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
            onOpenProfile={() => setView({ kind: 'profile' })}
          />
        )}
        {view.kind === 'profile' && <Profile dataVersion={dataVersion} onChanged={setProfile} />}
        {view.kind === 'trash' && (
          <Trash
            dataVersion={dataVersion}
            onOpenDoc={openDoc}
            onChanged={() => api<Doc[]>('/api/docs').then(setDocs).catch(() => {})}
          />
        )}
        {view.kind === 'graph' && (
          <Suspense fallback={<Loading />}>
            <GraphView onOpenDoc={openDoc} />
          </Suspense>
        )}
      </main>

      {profile && !profile.confirmed && <FirstRunName profile={profile} onSaved={setProfile} />}
      <ImportFolder
        hiddenButton
        inputId="import-folder-input"
        onDone={() => api<Doc[]>('/api/docs').then(setDocs).catch(() => {})}
      />

      {chipText(queueCount, view.kind === 'sharing' ? 0 : offerCount) && view.kind !== 'review' && (
        <button
          className="queue-chip"
          onClick={() => setView({ kind: queueCount > 0 ? 'review' : 'sharing' })}
          title={offerCount > 0 && queueCount > 0 ? 'share requests are on the Shares page' : undefined}
        >
          {chipText(queueCount, view.kind === 'sharing' ? 0 : offerCount)}
        </button>
      )}

      {palette === 'commands' && (
        <CommandPalette
          queueCount={queueCount}
          onAction={(a) => {
            if (a === 'review') setView({ kind: 'review' })
            if (a === 'runs') setView({ kind: 'runs' })
            if (a === 'graph') setView({ kind: 'graph' })
            if (a === 'sharing') setView({ kind: 'sharing' })
            if (a === 'profile') setView({ kind: 'profile' })
            if (a === 'trash') setView({ kind: 'trash' })
            if (a === 'ask') {
              setPalette('ask')
              return
            }
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
      {palette === 'open' && (
        <OpenDocPalette docs={docs} onOpenDoc={openDoc} onClose={() => setPalette(null)} />
      )}
      {palette === 'search' && <SearchPalette onOpenDoc={openDoc} onClose={() => setPalette(null)} />}
      {palette === 'help' && <ShortcutHelp onClose={() => setPalette(null)} />}
      {palette === 'ask' && <AskPalette onOpenDoc={openDoc} onClose={() => setPalette(null)} />}
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

/* ---------- palettes (PaletteShell lives in ./PaletteShell) ---------- */

/** The full shortcut cheatsheet (opened with `?`). The home hint bar shows the
 * common few; this is the complete list, including the editor-only marks. */
function ShortcutHelp({ onClose }: { onClose: () => void }) {
  const groups: [string, [string, string][]][] = [
    [
      'Navigate',
      [
        ['⌘K', 'commands'],
        ['⌘O', 'open a doc'],
        ['⌘P', 'search (⌘F too; ⌘S in an editor just confirms autosave)'],
        ['⌘/', 'ask the vault — an answer doc with block citations'],
        ['⌘T', 'toggle file tree'],
        ['⌘W', 'home'],
        ['⌘[ / ⌘]', 'history back / forward'],
        ['⌘R', 'reload'],
      ],
    ],
    [
      'Create & act',
      [
        ['⌘N', 'new doc'],
        ['⌘⇧N', 'new canvas'],
        ['⌘⇧R', 'review queue'],
        ['⌘G', 'gardeners'],
      ],
    ],
    [
      'In a doc',
      [
        ['⌘B / ⌘I / ⌘E', 'bold / italic / code'],
        ['⌘↵', 'save now / post comment'],
        ['Esc', 'close panel'],
        ['?', 'this cheatsheet'],
      ],
    ],
  ]
  return (
    <PaletteShell onClose={onClose}>
      <div className="shortcut-help">
        <div className="shortcut-help-title">Keyboard shortcuts</div>
        {groups.map(([title, rows]) => (
          <div className="shortcut-group" key={title}>
            <div className="shortcut-group-title">{title}</div>
            {rows.map(([keys, label]) => (
              <div className="shortcut-row" key={keys}>
                <span className="shortcut-keys">
                  {keys.split(' ').map((k, i) =>
                    k === '/' ? (
                      <span key={i}> / </span>
                    ) : (
                      <kbd key={i}>{k}</kbd>
                    ),
                  )}
                </span>
                <span className="shortcut-label">{label}</span>
              </div>
            ))}
          </div>
        ))}
      </div>
    </PaletteShell>
  )
}

function CommandPalette({
  queueCount,
  onAction,
  onClose,
}: {
  queueCount: number
  onAction: (
    a: 'review' | 'runs' | 'tree' | 'home' | 'newdoc' | 'newcanvas' | 'graph' | 'sharing' | 'profile' | 'trash' | 'close' | 'ask',
  ) => void
  onClose: () => void
}) {
  const [q, setQ] = useState('')
  const [sel, setSel] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  useEffect(() => inputRef.current?.focus(), [])

  type Item = { label: string; hint?: string; run: () => void }
  const commands: Item[] = [
    { label: `Review queue`, hint: '⌘⇧R', run: () => onAction('review') },
    { label: 'New doc…', hint: '⌘N', run: () => onAction('newdoc') },
    { label: 'New canvas…', hint: '⌘⇧N', run: () => onAction('newcanvas') },
    { label: 'Gardeners', hint: '⌘G', run: () => onAction('runs') },
    { label: 'Shares & contacts', run: () => onAction('sharing') },
    { label: 'Profile', hint: 'your name, node id, fingerprint', run: () => onAction('profile') },
    { label: 'Graph view', run: () => onAction('graph') },
    { label: 'Ask the vault…', hint: '⌘/ — an answer with citations', run: () => onAction('ask') },
    { label: 'Trash', hint: 'restore deleted docs', run: () => onAction('trash') },
    {
      label: 'Import a folder of Markdown…',
      hint: 'files become docs, folders become sections',
      run: () => {
        onAction('close')
        document.getElementById('import-folder-input')?.click()
      },
    },
    {
      label: 'Sync Claude Code memory now',
      hint: '~/.claude/projects/*/memory → Claude Memory (also runs every 10 min)',
      run: () => {
        onAction('close')
        api<{ files: number; imported: number; updated: number; unchanged: number; projects: number }>('/api/memory/sync', { method: 'POST' })
          .then((r) =>
            notify(
              `memory: ${r.files} files across ${r.projects} projects — ${r.imported} imported, ${r.updated} updated (in review), ${r.unchanged} unchanged`,
              'ok',
              { ttlMs: 10_000 },
            ),
          )
          .catch((e) => notify(errText(e)))
      },
    },
    {
      label: 'Export all docs as Markdown…',
      hint: 'a folder in ~/Downloads',
      run: () => {
        onAction('close')
        api<{ path: string; files: number }>('/api/export_vault', { method: 'POST' })
          .then((r) => notify(`exported ${r.files} files to ${r.path}`, 'ok', { ttlMs: 12_000 }))
          .catch((e) => notify(errText(e)))
      },
    },
    {
      label: 'Back up database now',
      hint: 'daily snapshot, kept beside your notes',
      run: () => {
        onAction('close')
        api<{ path: string; bytes: number }>('/api/backups', { method: 'POST' })
          .then((r) => notify(`backup written: ${r.path} (${(r.bytes / 1_048_576).toFixed(1)} MB)`, 'ok', { ttlMs: 12_000 }))
          .catch((e) => notify(errText(e)))
      },
    },
    { label: 'Toggle file tree', hint: '⌘T', run: () => onAction('tree') },
    { label: 'Home', run: () => onAction('home') },
  ]

  // ⌘K is operations only — docs live under ⌘O. Filter the commands, and
  // surface the live review count as a hint on that row.
  const items: Item[] = useMemo(() => {
    const needle = q.trim().toLowerCase()
    return commands
      .map((c) =>
        c.label === 'Review queue'
          ? { ...c, hint: queueCount ? `${queueCount} open` : c.hint }
          : c,
      )
      .filter((c) => !needle || c.label.toLowerCase().includes(needle))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q, queueCount])

  useEffect(() => setSel(0), [q])

  return (
    <PaletteShell onClose={onClose}>
      <input
        ref={inputRef}
        placeholder="Type a command…"
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

/** ⌘O — open a doc by title. Fuzzy over doc TITLES only (operations live under
 * ⌘K, block-content search under ⌘P). */
function OpenDocPalette({
  docs,
  onOpenDoc,
  onClose,
}: {
  docs: Doc[]
  onOpenDoc: (id: string) => void
  onClose: () => void
}) {
  const [q, setQ] = useState('')
  const [sel, setSel] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  useEffect(() => inputRef.current?.focus(), [])

  const hits = useMemo(() => {
    const needle = q.trim().toLowerCase()
    const matched = needle
      ? docs.filter((d) => fuzzyMatch(needle, d.title.toLowerCase()))
      : docs
    return matched.slice(0, 12)
  }, [q, docs])

  useEffect(() => setSel(0), [q])

  return (
    <PaletteShell onClose={onClose}>
      <input
        ref={inputRef}
        placeholder="Open a doc…"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown') setSel((s) => Math.min(s + 1, hits.length - 1))
          if (e.key === 'ArrowUp') setSel((s) => Math.max(s - 1, 0))
          if (e.key === 'Enter' && hits[sel]) onOpenDoc(hits[sel].id)
        }}
      />
      <div className="palette-list">
        {hits.map((d, i) => (
          <div
            key={d.id}
            className={`palette-item ${i === sel ? 'sel' : ''}`}
            onMouseEnter={() => setSel(i)}
            onClick={() => onOpenDoc(d.id)}
          >
            <span>{d.is_canvas ? `▨ ${d.title}` : d.title}</span>
            <span className="hint">{d.is_canvas ? 'canvas' : 'doc'}</span>
          </div>
        ))}
        {docs.length === 0 && <div className="palette-empty">no docs yet — ⌘N creates one</div>}
      </div>
    </PaletteShell>
  )
}

/** Ask the vault (⌘/): a question becomes an answer doc under Answers whose
 * every claim links [[Doc#^block]]. One round trip; the palette waits. */
function AskPalette({ onOpenDoc, onClose }: { onOpenDoc: OpenDoc; onClose: () => void }) {
  const [q, setQ] = useState('')
  const [busy, setBusy] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)
  useEffect(() => inputRef.current?.focus(), [])
  const submit = async () => {
    const question = q.trim()
    if (!question || busy) return
    setBusy(true)
    try {
      const a = await api<{ doc_id: string | null; title: string; sources: number; docs: number }>('/api/ask', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ question }),
      })
      if (!a.doc_id) {
        notify('nothing in your notes matches that yet', 'warn')
        setBusy(false)
        return
      }
      notify(`answered from ${a.sources} block${a.sources === 1 ? '' : 's'} across ${a.docs} doc${a.docs === 1 ? '' : 's'}`, 'ok')
      onOpenDoc(a.doc_id)
    } catch (e) {
      notify(errText(e))
      setBusy(false)
    }
  }
  return (
    <PaletteShell onClose={onClose} locked={busy}>
      <input
        ref={inputRef}
        placeholder="Ask your notes a question… every claim in the answer cites the block it came from"
        value={q}
        disabled={busy}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault()
            submit()
          }
        }}
      />
      <div className="palette-list">
        <div className="palette-empty">
          {busy ? '🌿 reading your notes and writing the answer… (up to a minute)' : 'Enter to ask · the answer lands as a doc under Answers'}
        </div>
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

/** rows rendered in the search palette; arrow keys clamp to these */
const PALETTE_MAX = 12

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
    // latest wins: a slow response for an older query must not overwrite
    // the hits for what is in the box now
    let stale = false
    const t = setTimeout(() => {
      api<SearchHit[]>(`/api/search?q=${encodeURIComponent(q)}`)
        .then((hs) => {
          if (!stale) setHits(hs)
        })
        .catch(() => {
          if (!stale) setHits([])
        })
    }, 120)
    return () => {
      stale = true
      clearTimeout(t)
    }
  }, [q])

  useEffect(() => setSel(0), [hits])
  const shown = hits.slice(0, PALETTE_MAX)

  return (
    <PaletteShell onClose={onClose}>
      <input
        ref={inputRef}
        placeholder="Search everything… (typos fine)"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown') setSel((s) => Math.min(s + 1, shown.length - 1))
          if (e.key === 'ArrowUp') setSel((s) => Math.max(s - 1, 0))
          if (e.key === 'Enter' && shown[sel]) onOpenDoc(shown[sel].block.doc_id)
        }}
      />
      <div className="palette-list">
        {shown.map((h, i) => (
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
    // the server's order: sort_key, NULL last (stable → ties keep its order)
    for (const kids of m.values()) kids.sort((a, b) => compareSortKey(a.sort_key, b.sort_key))
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
      const kids = (childrenOf.get(target.id) ?? []).filter((d) => d.id !== dragged)
      sortKey = keyForPosition(kids, kids.length)
      setOpenDirs((s) => new Set(s).add(target.id))
    } else {
      parent = target.parent_id
      const siblings = (childrenOf.get(parent) ?? []).filter((d) => d.id !== dragged)
      const i = siblings.findIndex((d) => d.id === target.id)
      sortKey = keyForPosition(siblings, mode === 'before' ? i : i + 1)
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
    try {
      const out = await api<{ deleted: number }>(`/api/doc/${d.id}/delete`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: '{}',
      })
      // nothing is gone for good (Trash), but the undo should be one click
      const inside = out.deleted > 1 ? ` and ${out.deleted - 1} inside it` : ''
      notify(`deleted “${d.title}”${inside} — click to undo`, 'ok', {
        ttlMs: 10_000,
        onClick: () => {
          restoreDoc(d.id)
            .then(() => {
              notify(`restored “${d.title}”`, 'ok')
              onChanged()
            })
            .catch((e) => notify(errText(e)))
        },
      })
    } catch (e) {
      notify(errText(e))
    }
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
            {d.mirror_permission && !d.from_hub && (
              <span className="mirror-badge" title={`shared with you (${d.mirror_permission})`}>⇄</span>
            )}
            {d.from_hub && (
              <span
                className="mirror-badge hub"
                title={d.origin_owner_name ? `relayed by the hub · owned by ${d.origin_owner_name}` : 'a hub folder'}
              >
                ⌂
              </span>
            )}
            {d.is_shared && !d.published_to && <span className="shared-badge" title="you share this subtree">↗</span>}
            {d.published_to && !d.mirror_permission && (
              <span className="shared-badge hub" title={`published to ${d.published_to}`}>⌂</span>
            )}
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

/** GET /api/doc/{id}/hot/status — newer fields are optional (older daemons). */
interface HotStatus {
  hot: boolean
  frozen_epoch?: number
  editors?: number
  /** may THIS participant write in the session (authoritative when present) */
  can_write?: boolean
  /** owned docs only: session-wide "everyone can edit" vs "watch only" */
  viewers_write?: boolean
  /** owned docs only: the room's agent (agents in the room) */
  agent?: AgentStatus
}

function DocView({
  docId,
  onOpenDoc,
  docs,
  dataVersion,
  liveChange = null,
  anchor,
  reviewIntent = false,
}: {
  docId: string
  onOpenDoc: OpenDoc
  docs: Doc[]
  dataVersion: number
  /** owner nudged us that a doc changed (GET /api/events doc_changed);
   * bumps `n` each time so the same doc can nudge twice */
  liveChange?: { docId: string; n: number } | null
  anchor?: string | null
  /** opened from the review queue: open the rail on load */
  reviewIntent?: boolean
}) {
  const [tree, setTree] = useState<DocTree | null>(null)
  const [backlinks, setBacklinks] = useState<SearchHit[]>([])
  const [fed, setFed] = useState<DocFederation | null>(null)
  const [hot, setHot] = useState<HotDoc | null>(null)
  const mirrorRef = useRef<unknown>(null)
  const [panel, setPanel] = useState<'none' | 'history' | 'comments' | 'tend' | 'share' | 'review'>('none')
  // stale guard: DocView is keyed by docId (one instance per open doc), so a
  // fetch that resolves after unmount — or after a doc switch — is for a doc
  // that is no longer on screen. `gen` bumps on every docId change too, in
  // case a caller ever reuses the instance.
  const gen = useRef(0)
  useEffect(() => {
    const g = ++gen.current
    return () => {
      if (gen.current === g) gen.current++
    }
  }, [docId])
  const fresh = useCallback(
    (g: number) => gen.current === g,
    [],
  )
  // open review items for THIS doc (yellow = applied+flagged, red = parked)
  const [reviewItems, setReviewItems] = useState<QueueRow[]>([])
  const loadReview = useCallback(() => {
    const g = gen.current
    api<QueueRow[]>(`/api/doc/${docId}/review`)
      .then((r) => fresh(g) && setReviewItems(Array.isArray(r) ? r : []))
      .catch(() => fresh(g) && setReviewItems([]))
  }, [docId, fresh])
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
    const g = gen.current
    api<DocTree>(`/api/doc/${docId}`)
      .then((t) => fresh(g) && setTree(t))
      .catch(console.error)
    api<SearchHit[]>(`/api/doc/${docId}/backlinks`)
      .then((b) => fresh(g) && setBacklinks(b))
      .catch(() => fresh(g) && setBacklinks([]))
    api<DocFederation>(`/api/doc/${docId}/federation`)
      .then((f) => fresh(g) && setFed(f))
      .catch(() => fresh(g) && setFed(null))
  }, [docId, fresh])

  useEffect(() => {
    setTree(null)
    setPanel(reviewIntent ? 'review' : 'none')
    setCommentTarget(null)
    setHot(null)
    setHotCanWrite(undefined)
    setViewersWrite(undefined)
    setReviewItems([])
    ownEpoch.current = 0
    loadTree()
    loadReview()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadTree, loadReview])

  // focus heartbeat (adaptive sync): while this doc is open, tell the daemon
  // every 5s so a mirrored share is pulled at 5s instead of 120s. Owned docs
  // are a server-side no-op; an older daemon 404s — either way, ignore.
  useEffect(() => {
    const beat = () => api(`/api/doc/${docId}/focus`, { method: 'POST' }).catch(() => {})
    beat()
    const t = setInterval(beat, 5000)
    return () => clearInterval(t)
  }, [docId])

  // a later { review: true } open of the SAME doc still opens the rail
  useEffect(() => {
    if (reviewIntent) setPanel('review')
  }, [reviewIntent, anchor])

  useEffect(() => {
    if (dataVersion === 0) return
    loadReview()
  }, [dataVersion, loadReview])

  // after a resolve: content may have moved (yellow decline reverts, red
  // accept applies) — refetch, remount the editor if the epoch moved
  const treeRef = useRef<DocTree | null>(null)
  treeRef.current = tree
  const afterResolve = useCallback(() => {
    loadReview()
    const g = gen.current
    api<DocTree>(`/api/doc/${docId}`)
      .then((next) => {
        if (!fresh(g)) return
        const cur = treeRef.current
        if (!cur || next.doc.current_epoch !== cur.doc.current_epoch) {
          ownEpoch.current = Math.max(ownEpoch.current, next.doc.current_epoch)
          setEditorGen((g) => g + 1)
        }
        setTree(next)
      })
      .catch(() => {})
  }, [docId, loadReview, fresh])

  const highlightMap = useMemo(() => buildHighlightMap(reviewItems), [reviewItems])

  // live refresh: when the store changed and this doc moved past what our own
  // saves produced, reload it and remount the editor with the fresh content
  // (skipped while dirty — the pending autosave lands first, next tick catches up)
  const refreshFromStore = useCallback(() => {
    const cur = treeRef.current
    if (!cur) return
    const g = gen.current
    api<DocTree>(`/api/doc/${docId}`)
      .then((next) => {
        if (!fresh(g)) return
        const known = Math.max(cur.doc.current_epoch, ownEpoch.current)
        const dirty = document.querySelector('.save-state.dirty, .save-state.saving')
        if (next.doc.current_epoch > known && !dirty) {
          setTree(next)
          setEditorGen((g) => g + 1)
        } else if (next.doc.status !== cur.doc.status) {
          setTree(next)
        }
        api<SearchHit[]>(`/api/doc/${docId}/backlinks`)
          .then((b) => fresh(g) && setBacklinks(b))
          .catch(() => {})
        api<DocFederation>(`/api/doc/${docId}/federation`)
          .then((f) => fresh(g) && setFed(f))
          .catch(() => {})
      })
      .catch((e) => console.warn('doc refresh failed', docId, e))
  }, [docId, fresh])
  useEffect(() => {
    if (dataVersion === 0) return
    refreshFromStore()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dataVersion])

  // an owner nudge (doc_changed) for THIS doc: the grantee daemon has already
  // pulled it, so refresh now rather than on the next stamp tick. The pull may
  // still be in flight — the stamp poll catches that case a tick later.
  useEffect(() => {
    if (!liveChange || liveChange.docId !== docId) return
    refreshFromStore()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [liveChange])

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
    // unsaved cold edits: the autosave lands within ~1.2s — wait for it
    // rather than starting a session that would freeze it out
    for (let i = 0; i < 12; i++) {
      if (!document.querySelector('.save-state.dirty, .save-state.saving')) break
      await new Promise((res) => setTimeout(res, 250))
    }
    if (document.querySelector('.save-state.dirty, .save-state.saving')) {
      notify('your last edit has not saved yet — try again in a moment', 'warn')
      return
    }
    try {
      // fetch → start(base_epoch) → seed from that same tree. The daemon
      // refuses to CREATE a session from any epoch but the current one
      // (code stale_base) — for a mirror it pulls the owner first — so a
      // save or pull that races us means "refetch and retry", never "seed
      // stale text that the flatten then lands over newer edits".
      let fresh = await api<DocTree>(`/api/doc/${docId}`)
      let r: { frozen_epoch: number; seed: boolean } | null = null
      for (let attempt = 0; attempt < 4 && !r; attempt++) {
        try {
          r = await api<{ frozen_epoch: number; seed: boolean }>(
            `/api/doc/${docId}/hot/start`,
            {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ base_epoch: fresh.doc.current_epoch }),
            },
          )
        } catch (e) {
          if (!(e instanceof ApiError) || e.code !== 'stale_base' || attempt === 3) throw e
          await new Promise((res) => setTimeout(res, 400))
          fresh = await api<DocTree>(`/api/doc/${docId}`)
        }
      }
      if (!r) return
      setHotCanWrite(true) // we started it; the next status poll may refine
      setHot({
        docId,
        frozenEpoch: r.frozen_epoch,
        seed: r.seed,
        blocks: r.seed ? editableBlocksOf(fresh) : editable.blocks,
      })
    } catch (e) {
      notify(errText(e))
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [docId, editable])

  // view-share mirrors watch the owner's live session read-only: they join
  // when it is hot but never start or end one
  const isViewMirror = fed?.mirror?.permission === 'view'
  // whether THIS participant may write in the current live session. The
  // daemon's hot/status `can_write` is authoritative when present (and is
  // re-evaluated on every poll — a session can flip it); absent (older
  // daemon) we fall back to the mirror permission; owned docs are writable.
  const [hotCanWrite, setHotCanWrite] = useState<boolean | undefined>(undefined)
  const hotReadOnly = hotCanWrite !== undefined ? !hotCanWrite : isViewMirror
  // session = consent: on an OWNED doc, whether every share participant may
  // edit the live session (true) or only watch (false). Reported by
  // hot/status for owned docs; undefined for mirrors and older daemons.
  const [viewersWrite, setViewersWrite] = useState<boolean | undefined>(undefined)
  const [agentStatus, setAgentStatus] = useState<AgentStatus | null>(null)
  const toggleViewersWrite = useCallback(
    async (enabled: boolean) => {
      setViewersWrite(enabled) // optimistic; the status poll re-reads the truth
      try {
        const r = await api<{ ok?: boolean; viewers_write?: boolean }>(
          `/api/doc/${docId}/hot/viewers_write`,
          {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ enabled }),
          },
        )
        if (typeof r.viewers_write === 'boolean') setViewersWrite(r.viewers_write)
      } catch (e) {
        setViewersWrite(!enabled)
        notify(errText(e))
      }
    },
    [docId],
  )

  // a live session started elsewhere (second window, another instance,
  // recovered journal, or the OWNER of a doc mirrored to us): join it rather
  // than editing cold. And when TWO editors are typing the same cold doc,
  // escalate to a live session (P2.1 auto-hot) — only from a clean editor,
  // so no keystrokes are lost.
  // While COLD, re-check on a 5s clock too (not only when the store changes):
  // for a mirror the status is a federation round-trip that can time out on a
  // bad path, and a single missed answer used to mean "toast, click, no
  // banner, nothing ever retried". Now a missed join self-heals.
  const [coldTick, setColdTick] = useState(0)
  useEffect(() => {
    if (hot) return
    const t = setInterval(() => setColdTick((x) => x + 1), 5000)
    return () => clearInterval(t)
  }, [hot, docId])
  // auto-hot is OPT-IN: when two editors sit on the same cold doc we offer to
  // go live together (once per doc-open) instead of silently switching the
  // editor out from under both of them
  const autoHotOffered = useRef<string | null>(null)
  const liveHeldOff = useRef<string | null>(null)
  useEffect(() => {
    if (!tree || hot) return
    const g = gen.current
    api<HotStatus>(`/api/doc/${docId}/hot/status`)
      .then((st) => {
        if (!editable || !fresh(g)) return
        if (st.hot) {
          // someone else went live while this cold editor holds unsaved text.
          // Swapping editors now would fire the cold save into the freeze and
          // lose it — stay cold; the editor keeps retrying and lands the save
          // when the session ends, and the next tick joins.
          if (document.querySelector('.save-state.dirty, .save-state.saving')) {
            if (liveHeldOff.current !== docId) {
              liveHeldOff.current = docId
              notify(
                'a live session started on this doc — your unsaved edits will save when it ends',
                'warn',
                { ttlMs: 10_000 },
              )
            }
            return
          }
          liveHeldOff.current = null
          setHotCanWrite(typeof st.can_write === 'boolean' ? st.can_write : undefined)
          setViewersWrite(typeof st.viewers_write === 'boolean' ? st.viewers_write : undefined)
          setHot({
            docId,
            frozenEpoch: st.frozen_epoch ?? tree.doc.current_epoch,
            seed: false,
            blocks: editable.blocks,
          })
        } else if ((st.editors ?? 0) >= 2 && !isViewMirror && autoHotOffered.current !== docId) {
          autoHotOffered.current = docId
          notify('someone else is editing this doc too — go live together?', 'ok', {
            onClick: () => {
              const dirty = document.querySelector('.save-state.dirty, .save-state.saving')
              if (dirty) notify('finish saving first, then try again')
              else goLive()
            },
            ttlMs: 20_000,
          })
        }
      })
      .catch(() => {})
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tree, dataVersion, coldTick])

  // while live: poll hot/status on its own clock (session state such as the
  // owner's "watch only" toggle lives in memory and never moves the store
  // stamp). If the daemon says the session is gone (the owner ended it and
  // our socket close was missed), fall back to the cold view.
  useEffect(() => {
    if (!hot) return
    let cancelled = false
    const poll = () =>
      api<HotStatus>(`/api/doc/${docId}/hot/status`)
        .then((st) => {
          if (cancelled) return
          if (!st.hot) {
            setHot(null)
            setHotCanWrite(undefined)
            setViewersWrite(undefined)
            setEditorGen((g) => g + 1)
            loadTree()
            loadReview()
            return
          }
          // both flip live mid-session: can_write re-renders HotEditor's
          // editable state (it calls editor.setEditable), viewers_write the chip
          if (typeof st.can_write === 'boolean') setHotCanWrite(st.can_write)
          if (typeof st.viewers_write === 'boolean') setViewersWrite(st.viewers_write)
          setAgentStatus(st.agent ?? null)
        })
        .catch(() => {})
    poll()
    const t = setInterval(poll, 2500)
    return () => {
      cancelled = true
      clearInterval(t)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hot?.docId, hot?.frozenEpoch])

  if (!tree || !editable) return <div className="empty">…</div>

  // a canvas doc IS the canvas: full-stage React Flow editor, its own experience
  if (canvases.length > 0 && editable.blocks.length === 0) {
    return (
      <div className="canvas-doc">
        <div className="canvas-doc-head">
          <DocTitle doc={tree.doc} onRenamed={loadTree} />
          <span className="meta">canvas · epoch {tree.doc.current_epoch}</span>
        </div>
        <Suspense fallback={<Loading />}>
          <CanvasBlock block={canvases[0]} epoch={tree.doc.current_epoch} onSaved={loadTree} full />
        </Suspense>
      </div>
    )
  }

  const mirror = fed?.mirror ?? null
  mirrorRef.current = mirror
  const pendingProposals = (fed?.outbound ?? []).filter((o) => o.state === 'pending')
  // hub (slice 1): this doc is inside a subtree published to a hub
  const publishedHub = fed?.shares.find((s) => s.to_hub && s.state === 'active')?.petname ?? null

  return (
    <article className="doc" onClick={onStageClick}>
      {mirror && mirror.origin_owner_name && (
        <div className="mirror-banner">
          ⌂ <b>{mirror.owner_petname}</b> · owned by <b>{mirror.origin_owner_name}</b>
          {mirror.permission === 'view'
            ? ' · view only'
            : ` · your edits go to ${mirror.origin_owner_name} as suggestions`}
          {pendingProposals.length > 0 && (
            <span className="pending-chip">
              {pendingProposals.length} suggestion{pendingProposals.length > 1 ? 's' : ''} awaiting{' '}
              {mirror.origin_owner_name}
            </span>
          )}
        </div>
      )}
      {mirror && !mirror.origin_owner_name && (
        <div className="mirror-banner">
          {mirror.transferred_from_me ? (
            <>
              ⌂ owned by <b>{mirror.owner_petname}</b> (transferred from you)
            </>
          ) : (
            <>
              {mirror.from_hub ? '⌂' : '⇄'} shared by <b>{mirror.owner_petname}</b>
            </>
          )}
          {mirror.permission === 'view'
            ? ' · view only'
            : ` · your edits go to ${mirror.transferred_from_me ? mirror.owner_petname : 'them'} as suggestions`}
          {mirror.owner_tended && ` · 🌿 tended by ${mirror.owner_petname}`}
          {pendingProposals.length > 0 && (
            <span className="pending-chip">
              {pendingProposals.length} suggestion{pendingProposals.length > 1 ? 's' : ''} awaiting{' '}
              {mirror.owner_petname}
            </span>
          )}
        </div>
      )}
      <div className="doc-head">
        <span className="head-actions">
          {tree.doc.review_policy && <span className="meta policy">{tree.doc.review_policy}</span>}
          {!mirror && <StatusChip doc={tree.doc} onChanged={loadTree} />}
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
              title={publishedHub ? `published to ${publishedHub}` : undefined}
            >
              {publishedHub ? `⌂ published to ${publishedHub}` : (fed?.shares.length ?? 0) > 0 ? '↗ shared' : 'share'}
            </button>
          )}
          {!hot && editable && (!mirror || (mirror.permission === 'propose' && !mirror.origin_owner_name)) && (
            <button
              className="chip"
              title="start a live co-editing session"
              onClick={goLive}
            >
              ⚡ go live
            </button>
          )}
          {reviewItems.length > 0 && (
            <button
              className={`chip review-chip ${panel === 'review' ? 'on' : ''}`}
              title={hot ? 'review after the live session ends' : 'review changes in this doc'}
              disabled={!!hot}
              onClick={() => setPanel(panel === 'review' ? 'none' : 'review')}
            >
              ⚠ {reviewItems.length} to review
            </button>
          )}
        </span>
        {mirror ? (
          <h1 className="doc-title readonly">{tree.doc.title}</h1>
        ) : (
          <DocTitle doc={tree.doc} onRenamed={loadTree} />
        )}
      </div>
      {hot && reviewItems.length > 0 && (
        <div className="meta review-hot-note">
          ⚠ {reviewItems.length} change{reviewItems.length === 1 ? '' : 's'} to review · review after the live session ends
        </div>
      )}
      {hot ? (
        <HotEditor
          key={`hot:${docId}`}
          doc={hot}
          readOnly={hotReadOnly}
          canEnd={!isViewMirror}
          viewersWrite={mirror ? undefined : viewersWrite}
          onToggleViewersWrite={mirror ? undefined : toggleViewersWrite}
          agent={mirror ? undefined : (agentStatus ?? undefined)}
          onAsk={
            mirror
              ? undefined
              : async (instruction) => {
                  try {
                    await api(`/api/doc/${docId}/hot/ask`, {
                      method: 'POST',
                      headers: { 'Content-Type': 'application/json' },
                      body: JSON.stringify({ instruction }),
                    })
                    setAgentStatus((a) => ({ ...(a ?? { busy: false }), busy: true, last_error: null, last_ok: null }))
                  } catch (e) {
                    notify(errText(e))
                  }
                }
          }
          onEnded={() => {
            // Fetch the flattened tree FIRST, then swap it in and remount the
            // cold editor in the same tick. Remounting before the fetch
            // resolved froze the editor on the pre-session content (its
            // initial content is memoised per doc), so the owner saw the
            // grantee's live text "vanish" even though the flatten landed.
            setHotCanWrite(undefined)
            setViewersWrite(undefined)
            loadReview()
            const g = gen.current
            api<DocTree>(`/api/doc/${docId}`)
              .then((next) => {
                if (!fresh(g)) return
                ownEpoch.current = Math.max(ownEpoch.current, next.doc.current_epoch)
                setTree(next)
                setHot(null)
                setEditorGen((g) => g + 1)
                api<SearchHit[]>(`/api/doc/${docId}/backlinks`)
                  .then((b) => fresh(g) && setBacklinks(b))
                  .catch(() => {})
                api<DocFederation>(`/api/doc/${docId}/federation`)
                  .then((f) => fresh(g) && setFed(f))
                  .catch(() => {})
              })
              .catch((e) => {
                if (!fresh(g)) return
                notify(`session ended but the doc could not be reloaded: ${errText(e)}`)
                setHot(null)
                loadTree()
              })
          }}
        />
      ) : (
      <DocEditor
        key={`${docId}:${editorGen}`}
        doc={editable}
        reviewMap={highlightMap}
        // a relayed doc proposes through the hub, which carries it to the owner
        mode={mirror ? (mirror.permission === 'propose' ? 'propose' : 'readonly') : 'direct'}
        onSaved={(e, savedDocId) => {
          // the unmount flush of a PREVIOUS doc's editor reports its own id
          if (savedDocId !== docId) return
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
      {panel === 'review' && !hot && (
        <ReviewRail items={reviewItems} onChanged={afterResolve} onClose={() => setPanel('none')} />
      )}
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
  onOpenDoc: OpenDoc
  dataVersion: number
}) {
  const [rows, setRows] = useState<QueueRow[]>([])
  // open the doc with its review rail up, scrolled to the op's block
  const openInDoc = (r: QueueRow) =>
    onOpenDoc(r.item.annotation.doc_id, { review: true, blockId: targetBlockOf(r) ?? undefined })
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
      notify(errText(e))
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
          <div
            key={r.item.annotation.id}
            className={`card ${parked ? 'red' : 'yellow'} clickable`}
            title="open in doc"
            onClick={() => openInDoc(r)}
          >
            <div className="card-head">
              <span className={`verdict ${parked ? 'v-red' : 'v-yellow'}`}>
                {parked ? 'parked' : 'applied'}
              </span>
              <span className="card-doc">{r.doc_title}</span>
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
            <div className="actions" onClick={(e) => e.stopPropagation()}>
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
              <button className="chip open-in-doc" onClick={() => openInDoc(r)}>
                open in doc →
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
