// The app: the Lapis shell (sidebar, top bar, phone tab bar) around one
// screen, chosen by the URL (router.ts). App owns the shared data (the doc
// list, the review count, the store stamp poll), the palettes and the global
// keys; each screen lives in its own file (screens/, or the older top-level
// ones: Home, Trash, BookLibrary, Settings, Gardeners, GraphView).

import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react'
import Gardeners from './Gardeners'
import { FirstRunName, loadProfile } from './Profile'
import Trash from './Trash'
import ImportFolder from './ImportFolder'
import DocTreePanel from './DocTree'
import { notify, errText, Notices } from './Notice'
import { CAPTURE_EVENT, onShellEvent } from './tauri'
import Home from './Home'
import Capture from './Capture'
import OpenPalette from './OpenPalette'
import ActionsPalette from './ActionsPalette'
import SearchPalette from './SearchPalette'
import Settings from './Settings'
import Books from './BookLibrary'
import BookUpload from './BookUpload'
import ShortcutHelp from './ShortcutHelp'
import NewDocPalette from './NewDocPalette'
import DocView, { type Panel } from './screens/DocView'
import ReviewQueue from './screens/ReviewQueue'
import Library, { pendingByDoc } from './screens/Library'
import { isEmptyFolder } from './tree'
import SearchScreen from './screens/Search'
import { buildCommands } from './commands'
import { signOut, webSession } from './auth'
import { buildPlaces, docPath, loadRecentIds, pushRecentId, storeRecentIds, type PlaceId } from './omni'
import { resolveShortcut } from './shortcuts'
import { goBack, goForward, navigate, useRoute, type Route } from './router'
import Link from './Link'
import { Button, EmptyState, Spinner } from './lapis'
import Sidebar from './shell/Sidebar'
import TopBar from './shell/TopBar'
import TabBar from './shell/TabBar'
import WorkspaceMenu from './shell/WorkspaceMenu'
import WorkspaceMembers from './shell/WorkspaceMembers'
import { placeTitle, primaryNav, secondaryNav, tabNav, type ShellCounts } from './shell/nav'
import { useShellCounts } from './shell/useShellCounts'
import { loadSidebarOpen, storeSidebarOpen, loadWorkspaceSel, storeWorkspaceSel, PHONE_QUERY } from './shell/prefs'
import { api, ApiError, Doc, QueueRow, Profile as ProfileRow } from './types'
import type { Workspace, WorkspaceSel } from './workspaces'
import { workspaceColor, workspaceSummary } from './workspaces'

type Stamp = { stamp: number; build?: number; version?: string }

/** Every request on the 2.5s poll is bounded: a hung fetch used to hold
 * `inFlight` forever, so `misses` stopped counting and the down-detector
 * never fired for the one failure mode it exists for. */
const pollTimeout = () => AbortSignal.timeout(4000)

/** An editor with unsaved work — a deploy must not reload over it. */
const DIRTY_SELECTOR = '.save-state.dirty, .save-state.saving'
// force-graph (graph) is not part of the boot bundle: it loads on first use
const GraphView = lazy(() => import('./GraphView'))
const Loading = () => (
  <div className="lazy-loading">
    <Spinner />
  </div>
)

/** The palettes, one job each: `open` (⌘O docs + places), `actions` (⌘K
 * commands), `search` (⌘⇧F full text + ask). */
type Palette = null | 'open' | 'actions' | 'search' | 'newdoc' | 'help' | 'capture'

/** How a doc is opened: `anchor` is a [[Doc#fragment]] target (`^uuid` for a
 * block); `review` opens the in-editor review rail; `blockId` scrolls to that
 * block (sugar for anchor `^blockId`, and it lands in the URL). */
export interface OpenDocOpts {
  anchor?: string
  review?: boolean
  blockId?: string
}
export type OpenDoc = (id: string, opts?: string | OpenDocOpts) => void

/** Sign out of the browser session, then reload onto the sign-in screen. */
export function signOutAndReload(): void {
  signOut()
    .then(() => location.reload())
    .catch((e) => notify(`could not sign out: ${errText(e)}`))
}

function subscribePhone(cb: () => void) {
  try {
    const m = matchMedia(PHONE_QUERY)
    m.addEventListener('change', cb)
    return () => m.removeEventListener('change', cb)
  } catch {
    return () => {}
  }
}
function isPhoneNow() {
  try {
    return matchMedia(PHONE_QUERY).matches
  } catch {
    return false
  }
}

export default function App() {
  const { route, state: navState } = useRoute()
  // a heading anchor or `^block` to scroll to, and "open the review rail",
  // ride in history.state; a block id also lands in the URL (?block=)
  const anchor = route.kind === 'doc' ? (navState.anchor ?? (route.block ? `^${route.block}` : null)) : null
  const reviewIntent = route.kind === 'doc' && !!navState.review
  // which side panel a doc shows. Above DocView because that is keyed by
  // docId: kept here, the rail the user opened survives a doc switch.
  const [docPanel, setDocPanel] = useState<Panel>('none')
  // the poll's own reply carries version/build/git: no page needs its own
  // /api/buildinfo round-trip for them
  const [appStamp, setAppStamp] = useState<Stamp | null>(null)
  const go = useCallback((r: Route) => navigate(r), [])

  const [docs, setDocs] = useState<Doc[]>([])
  const [docsLoaded, setDocsLoaded] = useState(false)
  const reloadDocs = useCallback(() => api<Doc[]>('/api/docs').then(setDocs).catch(() => {}), [])
  const [palette, setPalette] = useState<Palette>(null)
  // what Search opens with: `?` from ⌘/, the typed text from ⌘O's ⌘↵
  const [searchInitial, setSearchInitial] = useState('')
  // a palette's own key again closes it; another palette's key switches
  const togglePalette = useCallback((p: Exclude<Palette, null>) => {
    setPalette((cur) => (cur === p ? null : p))
  }, [])
  // `toggle`: the same key again (⌘⇧F, or ⌘/ on an ask) closes it; ⌘/ on
  // an open search switches it to ask
  const searchInitialRef = useRef('')
  const openSearch = useCallback((initial: string, toggle = true) => {
    const same = searchInitialRef.current === initial
    searchInitialRef.current = initial
    setSearchInitial(initial)
    setPalette((cur) => (toggle && same && cur === 'search' ? null : 'search'))
  }, [])
  const [queueCount, setQueueCount] = useState(0)
  // first-run name prompt: shown until the install-default name is confirmed.
  // null = no profile route (older daemon) or not loaded yet → no prompt.
  const [profile, setProfile] = useState<ProfileRow | null>(null)
  useEffect(() => {
    loadProfile().then(setProfile)
  }, [])
  // SERVER mode: signed in through the browser (offer Sign out); LOCAL: never
  const [signedIn, setSignedIn] = useState(false)
  // SERVER mode at all (the session route answers): the EPUB library lives
  // only there — LOCAL has no books routes, so its commands and views hide
  const [server, setServer] = useState(false)
  useEffect(() => {
    webSession().then((w) => {
      setSignedIn(!!w?.signed_in)
      setServer(!!w)
    })
  }, [])

  // the queue rows themselves feed the Library's "changes waiting" lines
  // and the tree's review dots
  const [queueRows, setQueueRows] = useState<QueueRow[]>([])
  const pendingDocs = useMemo(() => pendingByDoc(queueRows), [queueRows])
  const refreshQueue = useCallback(() => {
    Promise.all([
      api<QueueRow[]>('/api/queue')
        .then((q) => {
          const rows = Array.isArray(q) ? q : []
          setQueueRows(rows)
          return rows.length
        })
        .catch(() => 0),
      api<{ block: { id: string } }[]>('/api/flags').then((f) => f.length).catch(() => 0),
    ]).then(([q, f]) => setQueueCount(q + f))
  }, [])

  // first load: the doc list; a deep link to a doc that is not here says so
  const bootRoute = useRef(route)
  useEffect(() => {
    api<Doc[]>('/api/docs')
      .then((list) => {
        setDocs(list)
        setDocsLoaded(true)
        const r = bootRoute.current
        if (r.kind === 'doc' && !list.some((d) => d.id === r.id)) {
          notify('That doc is not here — it may have been deleted or moved to the Trash')
        }
      })
      .catch(console.error)
    refreshQueue()
  }, [refreshQueue])

  // live data: poll the store's change stamp; when it moves, every mounted
  // view refreshes (dataVersion threads down). Cheap — one local SQLite read.
  const [dataVersion, setDataVersion] = useState(0)
  // the stamp poll doubles as the liveness check: three misses in a row
  // (7.5s) = the background service is down; one hit clears it
  const [daemonDown, setDaemonDown] = useState(false)
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
          reloadDocs()
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
      } catch (e) {
        // signed out (SERVER mode): the sign-in screen says so, not the banner
        if (e instanceof ApiError && e.code === 'unauthorized') return
        // restarting mid-deploy, or actually down — say so after 3 misses
        misses += 1
        if (misses >= 3) setDaemonDown(true)
      } finally {
        inFlight = false
      }
    }, 2500)
    return () => clearInterval(t)
  }, [refreshQueue, reloadDocs])

  /* ---------- shell state: sidebar, drawer, workspace ---------- */

  const isPhone = useSyncExternalStore(subscribePhone, isPhoneNow, () => false)
  const [sidebarOpen, setSidebarOpenRaw] = useState(loadSidebarOpen)
  const setSidebarOpen = useCallback((open: boolean) => {
    setSidebarOpenRaw(open)
    storeSidebarOpen(open)
  }, [])
  const [drawerOpen, setDrawerOpen] = useState(false)
  // bumped when ⌘T opens the sidebar: the tree's filter takes the caret
  const [focusFilter, setFocusFilter] = useState(0)
  const toggleSidebar = useCallback(() => {
    if (isPhoneNow()) {
      setDrawerOpen((o) => !o)
      return
    }
    setSidebarOpenRaw((o) => {
      storeSidebarOpen(!o)
      if (!o) setFocusFilter((n) => n + 1)
      return !o
    })
  }, [])
  // the drawer closes when you go somewhere
  useEffect(() => setDrawerOpen(false), [route])

  const [workspaces, setWorkspaces] = useState<Workspace[]>([])
  const loadWorkspaces = useCallback(() => {
    api<{ workspaces: Workspace[] }>('/api/workspaces')
      .then((r) => setWorkspaces(Array.isArray(r?.workspaces) ? r.workspaces : []))
      .catch(() => setWorkspaces([]))
  }, [])
  useEffect(loadWorkspaces, [loadWorkspaces, dataVersion])
  const [ws, setWsRaw] = useState<WorkspaceSel>(loadWorkspaceSel)
  const setWs = useCallback((sel: WorkspaceSel) => {
    setWsRaw(sel)
    storeWorkspaceSel(sel)
  }, [])
  // a selection that disappeared (unshared, deleted) falls back to All
  useEffect(() => {
    if (ws !== 'all' && ws !== 'unsorted' && workspaces.length > 0 && !workspaces.some((w) => w.id === ws)) setWs('all')
  }, [ws, workspaces, setWs])
  // /library/<ws> picks the workspace; picking one while on the Library moves the URL
  useEffect(() => {
    if (route.kind === 'library') {
      const want = route.workspace ?? 'all'
      if (want !== ws) setWs(want)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [route])
  const pickWorkspace = useCallback(
    (sel: WorkspaceSel) => {
      setWs(sel)
      if (route.kind === 'library') navigate({ kind: 'library', workspace: sel === 'all' ? null : sel }, { replace: true })
    },
    [route, setWs],
  )
  const [membersOf, setMembersOf] = useState<Workspace | null>(null)
  const wsColor = useMemo(() => new Map(workspaces.map((w, i) => [w.id, workspaceColor(w, i)])), [workspaces])

  const side = useShellCounts(dataVersion)
  const counts: ShellCounts = { review: queueCount, inbox: side.inbox, todos: side.todos, trash: side.trash, docs: docs.length }

  /* ---------- navigation helpers ---------- */

  const openDoc = useCallback<OpenDoc>((id, opts) => {
    const o: OpenDocOpts = typeof opts === 'string' ? { anchor: opts } : (opts ?? {})
    const state = { anchor: o.anchor, review: o.review || undefined }
    navigate(o.blockId ? { kind: 'doc', id, block: o.blockId } : { kind: 'doc', id }, {
      state: state.anchor || state.review ? state : undefined,
    })
    setPalette(null)
    // ⌘O's empty-query list
    storeRecentIds(pushRecentId(loadRecentIds(), id))
  }, [])

  // a doc opened by URL (link, back/forward, reload) counts as recent too
  useEffect(() => {
    if (route.kind === 'doc') storeRecentIds(pushRecentId(loadRecentIds(), route.id))
  }, [route])

  const goPlace = useCallback((id: PlaceId) => {
    setPalette(null)
    if (id === 'settings') navigate({ kind: 'settings', section: 'profile' })
    else if (id === 'ereaders') navigate({ kind: 'settings', section: 'ereaders' })
    else navigate({ kind: id })
  }, [])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null
      const inEditor = !!el?.closest?.('.ProseMirror')
      const typing = !!el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable)
      const action = resolveShortcut(e, { inEditor, typing })
      // not ours: the key keeps its default (browser, editor, field)
      if (!action) return
      // Esc isn't a combo — leave its default (blur inputs etc.) intact.
      if (action === 'escape') {
        // a locked palette (ask in flight, first-run name) owns its own Esc
        if (document.querySelector('.palette-backdrop[data-locked], .lp-scrim[data-locked]')) return
        setPalette(null)
        setDrawerOpen(false)
        return
      }
      // ours: ⌘O must not open the file dialog, ⌘, the browser's
      // preferences, ⌘P print
      e.preventDefault()
      // a locked palette (an ask in flight, the first-run name) is not
      // replaced by another one
      if (document.querySelector('.palette-backdrop[data-locked], .lp-scrim[data-locked]')) return
      switch (action) {
        case 'help':
          togglePalette('help')
          break
        case 'actions':
          togglePalette('actions')
          break
        case 'open':
          togglePalette('open')
          break
        case 'search':
          openSearch('')
          break
        case 'ask':
          openSearch('? ')
          break
        case 'autosaved':
          // ⌘S while writing means "save" to every editor user: the doc
          // autosaves, so just say so
          notify('autosaved', 'ok', { ttlMs: 1500 })
          break
        case 'settings':
          setPalette(null)
          go({ kind: 'settings', section: 'profile' })
          break
        case 'capture':
          togglePalette('capture')
          break
        case 'tree':
          toggleSidebar()
          break
        case 'newdoc':
          togglePalette('newdoc')
          break
        case 'review':
          go({ kind: 'review' })
          break
        case 'gardeners':
          go({ kind: 'runs' })
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
          go({ kind: 'home' })
          break
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [go, togglePalette, openSearch, toggleSidebar])

  const docId = route.kind === 'doc' ? route.id : null

  // the ⌘K action list
  const commands = useMemo(
    () =>
      buildCommands({
        signedIn,
        library: server,
        onSignOut: () => {
          setPalette(null)
          signOutAndReload()
        },
        docId,
        onAction: (a) => {
          switch (a) {
            case 'capture':
            case 'newdoc':
            case 'open':
            case 'help':
              setPalette(a)
              return
            case 'search':
              openSearch('', false)
              return
            case 'ask':
              openSearch('? ', false)
              return
            case 'books':
              go({ kind: 'books' })
              break
            case 'tree':
              toggleSidebar()
              break
            case 'share':
              setDocPanel('share')
              break
          }
          setPalette(null)
        },
      }),
    [signedIn, server, docId, go, openSearch, toggleSidebar],
  )

  // the ⌘O places
  const places = useMemo(() => buildPlaces({ library: server, queueCount }), [server, queueCount])

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
      window.history.replaceState(history.state, '', location.pathname + (rest ? `?${rest}` : '') + location.hash)
      open()
    }
    return () => {
      offShell()
      window.removeEventListener(CAPTURE_EVENT, open)
    }
  }, [])

  /* ---------- the crumb ---------- */

  const byId = useMemo(() => new Map(docs.map((d) => [d.id, d])), [docs])
  const crumb = useMemo(() => {
    if (route.kind === 'doc') {
      const d = byId.get(route.id)
      if (!d) return <span className="lp-crumb-here">Doc</span>
      const chain: Doc[] = []
      let cur: Doc | undefined = d
      let guard = 0
      while (cur && guard++ < 32) {
        chain.unshift(cur)
        cur = cur.parent_id ? byId.get(cur.parent_id) : undefined
      }
      return (
        <>
          {chain.slice(0, -1).map((c) => (
            <span key={c.id} className="lp-crumb-step">
              <Link to={{ kind: 'doc', id: c.id }}>{c.title}</Link>
              <span aria-hidden="true">/</span>
            </span>
          ))}
          <span className="lp-crumb-here" title={docPath(d, byId)}>
            {d.title}
          </span>
        </>
      )
    }
    if (route.kind === 'library' && route.folder) {
      // a folder's page: Library / its ancestors / the folder
      const chain: Doc[] = []
      let cur = byId.get(route.folder)
      let guard = 0
      while (cur && guard++ < 32) {
        chain.unshift(cur)
        cur = cur.parent_id ? byId.get(cur.parent_id) : undefined
      }
      const wsr = route.workspace
      return (
        <>
          <span className="lp-crumb-step">
            <Link to={{ kind: 'library', workspace: wsr }}>Library</Link>
            <span aria-hidden="true">/</span>
          </span>
          {chain.slice(0, -1).map((c) => (
            <span key={c.id} className="lp-crumb-step">
              <Link to={{ kind: 'library', workspace: wsr, folder: c.id }}>{c.title}</Link>
              <span aria-hidden="true">/</span>
            </span>
          ))}
          <span className="lp-crumb-here">{chain[chain.length - 1]?.title ?? 'Folder'}</span>
        </>
      )
    }
    if (route.kind === 'library') {
      const { name } = workspaceSummary(route.workspace ?? 'all', workspaces, 0)
      return (
        <>
          <span className="lp-crumb-step">
            <Link to={{ kind: 'library', workspace: null }}>Library</Link>
            {route.workspace && <span aria-hidden="true">/</span>}
          </span>
          {route.workspace && <span className="lp-crumb-here">{name}</span>}
        </>
      )
    }
    return <span className="lp-crumb-here">{placeTitle(route)}</span>
  }, [route, byId, workspaces])

  // the tab title follows the place
  useEffect(() => {
    const d = route.kind === 'doc' ? byId.get(route.id) : undefined
    const what = route.kind === 'doc' ? (d?.title ?? 'Doc') : route.kind === 'home' ? null : placeTitle(route)
    document.title = what ? `${what} · Taisce` : 'Taisce'
  }, [route, byId])

  /* ---------- the screen ---------- */

  let screen: React.ReactNode
  switch (route.kind) {
    case 'home':
      screen = (
        <div className={`home ${docs.length === 0 ? '' : 'briefing'}`}>
          {docsLoaded && docs.length === 0 ? (
            <EmptyState
              icon="doc"
              title="Welcome to Taisce"
              action={
                <>
                  <Button variant="primary" icon="plus" kbd="⌘N" onClick={() => setPalette('newdoc')}>
                    New doc
                  </Button>
                  <ImportFolder label="Import a folder of Markdown…" onDone={reloadDocs} />
                </>
              }
            >
              Write your first doc, or bring your notes along. Press <kbd className="lp-kbd">?</kbd> for every shortcut.
            </EmptyState>
          ) : (
            <Home docs={docs} onOpenDoc={openDoc} dataVersion={dataVersion} onDocsChanged={reloadDocs} onOpenBooks={() => go({ kind: 'books' })} />
          )}
        </div>
      )
      break
    case 'doc':
      screen = (
        <DocView
          key={route.id}
          docId={route.id}
          onOpenDoc={openDoc}
          docs={docs}
          dataVersion={dataVersion}
          anchor={anchor}
          reviewIntent={reviewIntent}
          panel={docPanel}
          setPanel={setDocPanel}
          signedIn={signedIn}
        />
      )
      break
    case 'library':
      screen = (
        <Library
          docs={docs}
          workspace={route.workspace ?? 'all'}
          folder={route.folder ?? null}
          workspaces={workspaces}
          queue={queueRows}
          dataVersion={dataVersion}
          userName={profile?.name ?? null}
          onChanged={reloadDocs}
        />
      )
      break
    case 'review':
      screen = <ReviewQueue onChange={setQueueCount} onOpenDoc={openDoc} dataVersion={dataVersion} />
      break
    case 'search':
      screen = (
        <SearchScreen
          q={route.q}
          onOpenDoc={openDoc}
          docs={docs}
          workspaces={workspaces}
          dataVersion={dataVersion}
          askIntent={!!navState.ask}
          answerId={navState.answer ?? null}
        />
      )
      break
    case 'runs':
      screen = <Gardeners dataVersion={dataVersion} server={server} />
      break
    case 'settings':
      screen = (
        <Settings
          section={route.section}
          server={server}
          onSection={(section) => go({ kind: 'settings', section })}
          dataVersion={dataVersion}
          version={appStamp?.version ?? null}
          onProfileChanged={setProfile}
          onSignOut={signedIn ? signOutAndReload : undefined}
        />
      )
      break
    case 'trash':
      screen = <Trash dataVersion={dataVersion} onOpenDoc={openDoc} onChanged={reloadDocs} />
      break
    case 'books':
      screen = server ? <Books dataVersion={dataVersion} /> : <NotHere what="The book library lives on the Taisce server; this app has none." />
      break
    case 'graph':
      screen = (
        <Suspense fallback={<Loading />}>
          <GraphView onOpenDoc={openDoc} />
        </Suspense>
      )
      break
    case 'notfound':
      screen = <NotHere what={`Nothing lives at ${route.path}.`} />
      break
  }

  const showSidebar = isPhone ? drawerOpen : sidebarOpen
  const sidebar = showSidebar && (
    <Sidebar
      route={route}
      drawer={isPhone}
      primary={primaryNav({ server, counts, inboxDoc: side.inboxDoc, todoDoc: side.todoDoc })}
      secondary={secondaryNav(counts)}
      workspaceMenu={<WorkspaceMenu value={ws} list={workspaces} docCount={docs.length} onChange={pickWorkspace} onMembers={setMembersOf} onCreated={loadWorkspaces} />}
      tree={
        <DocTreePanel
          docs={docs}
          selected={docId}
          onSelect={(id) => {
            // a folder never written in opens as its folder page (canvas
            // Library); anything with its own text opens in the editor
            const d = byId.get(id)
            if (d && isEmptyFolder(d)) navigate({ kind: 'library', workspace: ws === 'all' ? null : ws, folder: id })
            else openDoc(id)
          }}
          pending={pendingDocs}
          selectedFolder={route.kind === 'library' ? (route.folder ?? null) : null}
          onChanged={reloadDocs}
          workspace={ws}
          wsColor={wsColor}
          onNewDoc={() => setPalette('newdoc')}
          focusFilter={focusFilter}
        />
      }
      userName={profile?.name ?? null}
      host={location.host}
      onHide={() => (isPhone ? setDrawerOpen(false) : setSidebarOpen(false))}
      onNavigate={() => setDrawerOpen(false)}
    />
  )

  return (
    <div className={`lp-app${showSidebar && !isPhone ? '' : ' lp-app-noside'}`}>
      {/* the window's title bar is transparent and the page runs under it
          (traffic lights float over the UI); this strip is what you grab
          to drag the window, and double-click to zoom it. Inert in a
          browser tab. */}
      <div className="titlebar" data-tauri-drag-region aria-hidden="true" />
      <a href="#main" className="lp-skip">
        Skip to content
      </a>
      <Notices />
      {daemonDown && (
        <div className="daemon-banner" role="status" data-tauri-drag-region>
          Taisce’s background service is not responding — edits are kept in the editor and will save when it is back
        </div>
      )}
      {sidebar &&
        (isPhone ? (
          <div className="lp-drawer-scrim" onMouseDown={() => setDrawerOpen(false)}>
            <div onMouseDown={(e) => e.stopPropagation()}>{sidebar}</div>
          </div>
        ) : (
          sidebar
        ))}
      <div className="lp-main">
        <TopBar
          crumb={crumb}
          sidebarHidden={!isPhone && !sidebarOpen}
          onShowSidebar={() => (isPhone ? setDrawerOpen(true) : setSidebarOpen(true))}
          onOpen={() => togglePalette('open')}
          onActions={() => togglePalette('actions')}
          onCapture={() => togglePalette('capture')}
          onNewDoc={() => togglePalette('newdoc')}
          onBack={goBack}
          onForward={goForward}
        />
        <main id="main" className={`stage stage-${route.kind}`} tabIndex={-1} onClick={() => palette && setPalette(null)}>
          {screen}
        </main>
      </div>
      <TabBar items={tabNav({ server, counts })} route={route} onCapture={() => togglePalette('capture')} />

      {profile && !profile.confirmed && <FirstRunName profile={profile} onSaved={setProfile} />}
      <ImportFolder hiddenButton inputId="import-folder-input" onDone={reloadDocs} />
      {/* the ⌘K "Add EPUBs…" command clicks this input and shows the Books
          view, which renders the shared upload queue's chips and cards */}
      {server && <BookUpload hiddenButton inputId="book-upload-input" />}
      {membersOf && <WorkspaceMembers workspace={membersOf} onClose={() => setMembersOf(null)} onChanged={loadWorkspaces} />}

      {palette === 'open' && (
        <OpenPalette
          docs={docs}
          places={places}
          currentId={docId}
          onOpenDoc={openDoc}
          onPlace={goPlace}
          onSearchText={(q) => openSearch(q, false)}
          onClose={() => setPalette(null)}
        />
      )}
      {palette === 'actions' && <ActionsPalette commands={commands} docTitle={docId ? byId.get(docId)?.title : null} onClose={() => setPalette(null)} />}
      {palette === 'search' && (
        <SearchPalette key={searchInitial} initial={searchInitial} onOpenDoc={openDoc} onClose={() => setPalette(null)} />
      )}
      {palette === 'help' && <ShortcutHelp onClose={() => setPalette(null)} />}
      {palette === 'capture' && (
        <Capture
          onClose={() => setPalette(null)}
          onCaptured={(c) => {
            reloadDocs()
            notify(`captured → Inbox · ${c.title}`, 'ok', { onClick: () => openDoc(c.doc_id) })
          }}
        />
      )}
      {palette === 'newdoc' && (
        <NewDocPalette
          onCreated={(id) => {
            reloadDocs()
            openDoc(id)
          }}
          onClose={() => setPalette(null)}
        />
      )}
    </div>
  )
}

function NotHere({ what }: { what: string }) {
  return (
    <div className="lp-page">
      <EmptyState
        icon="info"
        title="Nothing here"
        action={
          <Button variant="primary" icon="home" onClick={() => navigate({ kind: 'home' })}>
            Go home
          </Button>
        }
      >
        {what}
      </EmptyState>
    </div>
  )
}
