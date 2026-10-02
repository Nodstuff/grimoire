import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import DocEditor from './editor/DocEditor'
import TendPanel from './TendPanel'
import Gardeners from './Gardeners'
import PaletteShell from './PaletteShell'
import Profile, { FirstRunName, loadProfile } from './Profile'
import Trash from './Trash'
import LivingChip from './LivingChip'
import ImportFolder from './ImportFolder'
import ReviewRail from './ReviewRail'
import DocTreePanel from './DocTree'
import { notify, errText, Notices } from './Notice'
import { CAPTURE_EVENT, onShellEvent } from './tauri'
import Home from './Home'
import Capture from './Capture'
import Omnibox from './Omnibox'
import { buildCommands } from './commands'
import { loadRecentIds, pushRecentId, storeRecentIds, type OmniMode } from './omni'

/** The doc side panels, owned by App so a doc switch does not close one. */
type Panel = 'none' | 'history' | 'comments' | 'tend' | 'review'

type Stamp = { stamp: number; build?: number; version?: string }

/** Every request on the 2.5s poll is bounded: a hung fetch used to hold
 * `inFlight` forever, so `misses` stopped counting and the down-detector
 * never fired for the one failure mode it exists for. */
const pollTimeout = () => AbortSignal.timeout(4000)

/** An editor with unsaved work — a deploy must not reload over it. */
const DIRTY_SELECTOR = '.save-state.dirty, .save-state.saving'
// force-graph (graph) is not part of the boot bundle: it loads on first use
const GraphView = lazy(() => import('./GraphView'))
const Loading = () => <div className="lazy-loading">loading…</div>
import { resolveShortcut } from './shortcuts'
import { parseDeepLink, scrubDeepLink } from './deeplink'
import { actionLabels, buildHighlightMap, describeChange, isDocOp, targetBlockOf } from './review'
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
  Profile as ProfileRow,
} from './types'

type View =
  | { kind: 'doc'; id: string }
  | { kind: 'review' }
  | { kind: 'runs' }
  | { kind: 'graph' }
  | { kind: 'profile' }
  | { kind: 'trash' }
  | { kind: 'home' }
/** `omnibox` is the one search-and-command palette (⌘K / ⌘O / ⌘P / ⌘/ all
 * open it, in different modes — see `omniMode`). */
type Palette = null | 'omnibox' | 'newdoc' | 'help' | 'capture'

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
  const [anchor, setAnchor] = useState<string | null>(null)
  // opened FROM the review queue (or with { review: true }): DocView opens its rail
  const [reviewIntent, setReviewIntent] = useState(false)
  // which side panel a doc shows. Above DocView because that is keyed by
  // docId: kept here, the rail the user opened survives a doc switch.
  const [docPanel, setDocPanel] = useState<Panel>('none')
  // the poll's own reply carries version/build/git: no page needs its own
  // /api/buildinfo round-trip for them
  const [appStamp, setAppStamp] = useState<Stamp | null>(null)

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
  // which groups the omnibox leads with: ⌘K mixed, ⌘O docs, ⌘P content, ⌘/ ask
  const [omniMode, setOmniMode] = useState<OmniMode>('mixed')
  const omniModeRef = useRef<OmniMode>('mixed')
  const openOmnibox = useCallback((mode: OmniMode) => {
    setOmniMode(mode)
    // the same key again closes it; a different alias just switches mode
    setPalette((p) => (p === 'omnibox' && mode === omniModeRef.current ? null : 'omnibox'))
    omniModeRef.current = mode
  }, [])
  const [queueCount, setQueueCount] = useState(0)
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshQueue])

  // live data: poll the store's change stamp; when it moves, every mounted
  // view refreshes (dataVersion threads down). Cheap — one local SQLite read.
  const [dataVersion, setDataVersion] = useState(0)
  // the stamp poll doubles as the liveness check: three misses in a row
  // (7.5s) = the background service is down; one hit clears it
  const [daemonDown, setDaemonDown] = useState(false)
  const openDocRef = useRef<OpenDoc>(() => {})
  useEffect(() => {
    let stamp: number | null = null
    let build: number | null = null
    let misses = 0
    let inFlight = false
    const t = setInterval(async () => {
      // a slow daemon must not stack ticks (each one is up to two requests)
      if (inFlight) return
      inFlight = true
      try {
        const r = await api<Stamp>('/api/stamp', { signal: pollTimeout() })
        misses = 0
        setAppStamp(r)
        setDaemonDown(false)
        if (stamp === null) stamp = r.stamp
        else if (r.stamp !== stamp) {
          stamp = r.stamp
          setDataVersion((v) => v + 1)
          api<Doc[]>('/api/docs').then(setDocs).catch(() => {})
          refreshQueue()
        }
        // deploy landed → reload the bundle (deferred while an editor is dirty).
        // Newer daemons carry the build on the stamp; fall back to the
        // dedicated route only when it is absent.
        const b =
          typeof r.build === 'number'
            ? r.build
            : (await api<{ build: number }>('/api/buildinfo', { signal: pollTimeout() })).build
        if (build === null) build = b
        else if (b !== build && !document.querySelector(DIRTY_SELECTOR)) {
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

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const action = resolveShortcut(e)
      if (!action) return
      // Esc isn't a combo — leave its default (blur inputs etc.) intact.
      if (action === 'escape') {
        // a locked palette (ask in flight, first-run name) owns its own Esc
        if (document.querySelector('.palette-backdrop[data-locked]')) return
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
          openOmnibox('mixed')
          break
        case 'open':
          openOmnibox('docs')
          break
        case 'search':
          // ⌘S while writing means "save" to every editor user: the doc
          // autosaves, so just say so instead of opening search
          if (e.key.toLowerCase() === 's' && (e.target as HTMLElement | null)?.closest('.ProseMirror')) {
            notify('autosaved', 'ok', { ttlMs: 1500 })
            break
          }
          openOmnibox('content')
          break
        case 'ask':
          openOmnibox('ask')
          break
        case 'capture':
          setPalette((p) => (p === 'capture' ? null : 'capture'))
          break
        case 'tree':
          setTreeOpen((t) => !t)
          break
        case 'newdoc':
          setPalette((p) => (p === 'newdoc' ? null : 'newdoc'))
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
  }, [goBack, goForward, setView, openOmnibox])

  const openDoc = useCallback<OpenDoc>(
    (id, opts) => {
      const o: OpenDocOpts = typeof opts === 'string' ? { anchor: opts } : (opts ?? {})
      setAnchor(o.anchor ?? (o.blockId ? `^${o.blockId}` : null))
      setReviewIntent(!!o.review)
      setView({ kind: 'doc', id })
      setPalette(null)
      // the omnibox's empty-query list
      storeRecentIds(pushRecentId(loadRecentIds(), id))
    },
    [setView],
  )
  openDocRef.current = openDoc

  // the ⌘K command list (data for the omnibox's Commands group)
  const commands = useMemo(
    () =>
      buildCommands({
        queueCount,
        docId: view.kind === 'doc' ? view.id : null,
        inboxId: docs.find((d) => d.parent_id === null && d.title === 'Inbox')?.id ?? null,
        onOpenDoc: (id) => {
          setPalette(null)
          openDocRef.current(id)
        },
        onAction: (a) => {
          if (a === 'review') setView({ kind: 'review' })
          if (a === 'runs') setView({ kind: 'runs' })
          if (a === 'graph') setView({ kind: 'graph' })
          if (a === 'profile') setView({ kind: 'profile' })
          if (a === 'trash') setView({ kind: 'trash' })
          if (a === 'home') setView({ kind: 'home' })
          if (a === 'tree') setTreeOpen((t) => !t)
          if (a === 'capture' || a === 'newdoc') {
            setPalette(a)
            return
          }
          setPalette(null)
        },
      }),
    [queueCount, view, setView, docs],
  )

  // quick capture from outside the page: the shell's global hotkey (⌥⌘G) and
  // tray item emit CAPTURE_EVENT; a window the hotkey had to CREATE arrives
  // as ?capture=1 instead (the page was not there to hear an event). A DOM
  // event of the same name lets anything in-page open it too.
  useEffect(() => {
    const open = () => setPalette('capture')
    const offShell = onShellEvent(CAPTURE_EVENT, open)
    window.addEventListener(CAPTURE_EVENT, open)
    const params = new URLSearchParams(location.search)
    if (params.get('capture') === '1') {
      params.delete('capture')
      const rest = params.toString()
      window.history.replaceState(null, '', location.pathname + (rest ? `?${rest}` : '') + location.hash)
      open()
    }
    return () => {
      offShell()
      window.removeEventListener(CAPTURE_EVENT, open)
    }
  }, [])

  return (
    <div className="app">
      {/* the window's title bar is transparent and the page runs under it
          (traffic lights float over the UI); this strip is what you grab
          to drag the window, and double-click to zoom it. Inert in a
          browser tab. */}
      <div className="titlebar" data-tauri-drag-region aria-hidden="true" />
      <Notices />
      {daemonDown && (
        <div className="daemon-banner" role="status" data-tauri-drag-region>
          Taisce’s background service is not responding — edits are kept in the editor and will save when it is back
        </div>
      )}
      {treeOpen && (
        <DocTreePanel
          docs={docs}
          selected={view.kind === 'doc' ? view.id : null}
          onSelect={openDoc}
          onClose={() => setTreeOpen(false)}
          onChanged={() => api<Doc[]>('/api/docs').then(setDocs).catch(() => {})}
        />
      )}
      <main className="stage" onClick={() => palette && setPalette(null)}>
        {view.kind === 'home' && (
          <div className={`home ${docs.length === 0 ? '' : 'briefing'}`}>
            {docs.length === 0 && <div className="home-mark">◈</div>}
            {docs.length === 0 ? (
              <div className="home-start">
                <div className="home-start-title">Welcome to Taisce</div>
                <div><kbd>⌘N</kbd> create your first doc</div>
                <div>
                  <ImportFolder
                    label="Already have notes? Import a folder of Markdown…"
                    onDone={() => api<Doc[]>('/api/docs').then(setDocs).catch(() => {})}
                  />
                </div>
                <div><kbd>?</kbd> all shortcuts</div>
              </div>
            ) : (
              <Home
                docs={docs}
                onOpenDoc={openDoc}
                dataVersion={dataVersion}
                onDocsChanged={() => api<Doc[]>('/api/docs').then(setDocs).catch(() => {})}
              />
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
            anchor={anchor}
            reviewIntent={reviewIntent}
            panel={docPanel}
            setPanel={setDocPanel}
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
        {view.kind === 'profile' && (
          <Profile dataVersion={dataVersion} onChanged={setProfile} version={appStamp?.version ?? null} />
        )}
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

      {queueCount > 0 && view.kind !== 'review' && (
        <button className="queue-chip" onClick={() => setView({ kind: 'review' })}>
          {queueCount} to review
        </button>
      )}

      {palette === 'omnibox' && (
        <Omnibox
          mode={omniMode}
          docs={docs}
          commands={commands}
          onOpenDoc={openDoc}
          onClose={() => setPalette(null)}
        />
      )}
      {palette === 'help' && <ShortcutHelp onClose={() => setPalette(null)} />}
      {palette === 'capture' && (
        <Capture
          onClose={() => setPalette(null)}
          onCaptured={(c) => {
            api<Doc[]>('/api/docs').then(setDocs).catch(() => {})
            notify(`captured → Inbox · ${c.title}`, 'ok', { onClick: () => openDoc(c.doc_id) })
          }}
        />
      )}
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

/* ---------- palettes (PaletteShell lives in ./PaletteShell) ---------- */

/** The full shortcut cheatsheet (opened with `?`). The home hint bar shows the
 * common few; this is the complete list, including the editor-only marks. */
function ShortcutHelp({ onClose }: { onClose: () => void }) {
  const groups: [string, [string, string][]][] = [
    [
      'Navigate',
      [
        ['⌘K', 'omnibox — docs, content, commands and ask in one box (> commands · ? ask)'],
        ['⌘O', 'omnibox, docs first (recent when empty)'],
        ['⌘P', 'omnibox, content search (⌘F too; ⌘S in an editor just confirms autosave)'],
        ['⌘/', 'omnibox, ask the vault — an answer doc with block citations (⌘↵ asks from any row)'],
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
        ['⌘⇧I', 'quick capture → Inbox (⌥⌘G from anywhere on the Mac, in the app)'],
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

function NewDocPalette({ onCreated, onClose }: { onCreated: (id: string) => void; onClose: () => void }) {
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
          if (e.key === 'Enter') create().catch((err) => notify(String(err)))
        }}
      />
      <div className="palette-empty">Enter to create</div>
    </PaletteShell>
  )
}

/* ---------- tree ---------- */

/* ---------- doc view ---------- */

/** Click-to-rename doc title. Inbound [[wikilinks]] resolve by title and are
 * not rewritten on rename — they dangle until edited. */
function DocTitle({ doc, onRenamed }: { doc: Doc; onRenamed: () => void }) {
  const [editing, setEditing] = useState(false)
  const [value, setValue] = useState(doc.title)
  // Enter unmounts the input, whose blur fires save again: one rename only
  const saving = useRef(false)

  const save = async () => {
    if (saving.current) return
    saving.current = true
    setEditing(false)
    const title = value.trim()
    if (!title || title === doc.title) {
      setValue(doc.title)
      saving.current = false
      return
    }
    try {
      await api(`/api/doc/${doc.id}/rename`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ title }),
      })
    } catch (e) {
      console.error(e)
      notify(`rename failed: ${errText(e)}`, 'warn')
      setValue(doc.title)
    } finally {
      saving.current = false
    }
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
  reviewIntent = false,
  panel,
  setPanel,
}: {
  docId: string
  onOpenDoc: OpenDoc
  docs: Doc[]
  dataVersion: number
  anchor?: string | null
  /** opened from the review queue: open the rail on load */
  reviewIntent?: boolean
  /** owned by App so it outlives this component's per-doc remount */
  panel: Panel
  setPanel: (p: Panel) => void
}) {
  const [tree, setTree] = useState<DocTree | null>(null)
  const [backlinks, setBacklinks] = useState<SearchHit[]>([])
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
  }, [docId, fresh])

  useEffect(() => {
    setTree(null)
    // the panel choice is the user's, not the doc's: only a review-queue
    // open overrides it (the same rule as the reviewIntent effect below)
    if (reviewIntent) setPanel('review')
    setCommentTarget(null)
    setReviewItems([])
    ownEpoch.current = 0
    loadTree()
    loadReview()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadTree, loadReview])

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
      })
      .catch((e) => console.warn('doc refresh failed', docId, e))
  }, [docId, fresh])
  useEffect(() => {
    if (dataVersion === 0) return
    refreshFromStore()
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

  const { editable, comments, allBlocks, removedCanvases } = useMemo(() => {
    if (!tree)
      return {
        editable: null,
        comments: [] as Block[],
        allBlocks: [] as Block[],
        removedCanvases: 0,
      }
    const blocks: Block[] = []
    const comments: Block[] = []
    const allBlocks: Block[] = []
    let removedCanvases = 0
    const walk = (nodes: BlockNode[]) => {
      for (const n of nodes) {
        allBlocks.push(n.block)
        if (n.block.block_type === 'comment') comments.push(n.block)
        // the canvas editor is gone; old scenes stay in the store, unrendered
        else if (n.block.block_type === 'canvas_scene') removedCanvases++
        else if (!n.block.content.startsWith('---')) blocks.push(n.block)
        walk(n.children)
      }
    }
    walk(tree.roots)
    return {
      editable: { docId, epoch: tree.doc.current_epoch, blocks },
      comments,
      allBlocks,
      removedCanvases,
    }
  }, [tree, docId])

  if (!tree || !editable) return <div className="empty">…</div>

  return (
    <article className="doc" onClick={onStageClick}>
      <div className="doc-head">
        <span className="head-actions">
          {tree.doc.review_policy && <span className="meta policy">{tree.doc.review_policy}</span>}
          <LivingChip docId={docId} dataVersion={dataVersion} />
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
          <button
            className={`chip ${panel === 'tend' ? 'on' : ''} ${docs.find((d) => d.id === docId)?.is_tended ? 'tended' : ''}`}
            onClick={() => setPanel(panel === 'tend' ? 'none' : 'tend')}
          >
            {docs.find((d) => d.id === docId)?.is_tended ? '🌿 tended' : 'tend'}
          </button>
          {reviewItems.length > 0 && (
            <button
              className={`chip review-chip ${panel === 'review' ? 'on' : ''}`}
              title="review changes in this doc"
              onClick={() => setPanel(panel === 'review' ? 'none' : 'review')}
            >
              ⚠ {reviewItems.length} to review
            </button>
          )}
        </span>
        <DocTitle doc={tree.doc} onRenamed={loadTree} />
      </div>
      {removedCanvases > 0 && (
        <div className="meta canvas-removed">
          Canvas (removed) · this doc holds {removedCanvases === 1 ? 'a canvas' : `${removedCanvases} canvases`} the
          app no longer shows
        </div>
      )}
      <DocEditor
        key={`${docId}:${editorGen}`}
        doc={editable}
        reviewMap={highlightMap}
        onSaved={(e, savedDocId) => {
          // the unmount flush of a PREVIOUS doc's editor reports its own id
          if (savedDocId !== docId) return
          ownEpoch.current = Math.max(ownEpoch.current, e)
        }}
        onSelectionBlock={onSelectionBlock}
      />
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
      {panel === 'review' && (
        <ReviewRail items={reviewItems} onChanged={afterResolve} onClose={() => setPanel('none')} />
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
        // doc ops (an agent's rename / move / status / trash through the
        // gate) have no block diff: the card is the sentence describeChange builds
        const docOp = isDocOp(op.kind.op) ? describeChange(r) : null
        const labels = actionLabels(r)
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
            {docOp ? (
              <>
                <div className="rail-headline">{docOp.headline}</div>
                {(docOp.before || docOp.after) && (
                  <div className="diff">
                    {docOp.before && (
                      <div className="diff-col">
                        <div className="diff-label">{docOp.before.label}</div>
                        <pre>{docOp.before.text}</pre>
                      </div>
                    )}
                    {docOp.after && (
                      <div className="diff-col">
                        <div className="diff-label">{docOp.after.label}</div>
                        <pre>{docOp.after.text}</pre>
                      </div>
                    )}
                  </div>
                )}
              </>
            ) : (
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
            )}
            {op.source_refs.length > 0 && <div className="refs">{op.source_refs.join(' · ')}</div>}
            <div className="actions" onClick={(e) => e.stopPropagation()}>
              <button
                className="accept"
                disabled={busy === r.item.annotation.id}
                onClick={() => resolve(r.item.annotation.id, 'accept')}
              >
                {labels.accept}
              </button>
              <button
                className="decline"
                disabled={busy === r.item.annotation.id}
                onClick={() => resolve(r.item.annotation.id, 'decline')}
              >
                {labels.decline}
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
