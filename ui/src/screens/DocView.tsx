// The doc view (Lapis Doc / Mobile-Doc artboards): the head (workspace,
// status and tag pills, the Newsreader title, who wrote it), the editor, the
// "Linked from" cards, and the inspector (Review / Comments / History /
// Share) at the right. The save state and Share sit in the top bar (a portal
// into the shell's slot). On a phone the inspector is a bottom sheet opened
// from a dock that replaces the tab bar.
//
// The editor engine (DocEditor: Tiptap, autosave, collisions, markdown
// shortcuts) is unchanged; this file is the chrome around it.

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import DocEditor, { parser, type EditorSaveState } from '../editor/DocEditor'
import { splitTree } from '../editor/splitTree'
import LivingChip from '../LivingChip'
import EreaderChip from '../EreaderChip'
import { EREADER_EVENT, loadEreaderStatus, syncDiagramImages, useEreaderStatus } from '../ereader'
import { notify, errText } from '../Notice'
import { actionLabels, buildHighlightMap, targetBlockOf, toneOf } from '../review'
import { api, Block, Doc, DocTree, HistoryRow, QueueRow, SearchHit } from '../types'
import type { OpenDoc } from '../App'
import { navigate } from '../router'
import { docWorkspace, type Workspace } from '../workspaces'
import { keyBetween } from '../editor/diff'
import type { ReviewDetail, ReviewExtras } from '../editor/ReviewHighlight'
import { backlinkSnippet, contributors, frontmatterTags, STATUSES, statusTone, withTag } from '../editor/docMeta'
import { Avatar, Button, CountPill, Icon, IconButton, StatusDot, isAgentName, useFocusTrap, type SaveState } from '../lapis'
import { DocInspector, type InspectorTab, type Principal } from './DocInspector'
import './docview.css'

/** Which inspector tab a doc shows, owned by App so a doc switch keeps it.
 * 'none' = automatic (open on a wide window, on Review when something
 * waits, else Comments; closed on a narrow one); 'closed' = hidden by hand. */
export type Panel = 'none' | 'closed' | InspectorTab

type HistRow = HistoryRow & { principal_is_yours?: boolean }

/* ---------- small hooks and caches ---------- */

function useMedia(query: string): boolean {
  const get = () => {
    try {
      return matchMedia(query).matches
    } catch {
      return false
    }
  }
  const [m, setM] = useState(get)
  useEffect(() => {
    let mq: MediaQueryList
    try {
      mq = matchMedia(query)
    } catch {
      return
    }
    const on = () => setM(mq.matches)
    on()
    mq.addEventListener('change', on)
    return () => mq.removeEventListener('change', on)
  }, [query])
  return m
}

interface PrincipalRow {
  id: string
  kind: string
  display_name: string
}
let principalsP: Promise<Map<string, PrincipalRow>> | null = null
function loadPrincipals(force = false) {
  if (!principalsP || force) {
    principalsP = api<PrincipalRow[]>('/api/principals')
      .then((rows) => new Map((Array.isArray(rows) ? rows : []).map((p) => [p.id, p])))
      .catch(() => {
        principalsP = null
        return new Map()
      })
  }
  return principalsP
}
let meP: Promise<string | null> | null = null
function loadMe() {
  if (!meP)
    meP = api<{ principal_id: string }>('/api/profile')
      .then((p) => p.principal_id ?? null)
      .catch(() => {
        meP = null
        return null
      })
  return meP
}

const SAVE_OF: Record<EditorSaveState, SaveState> = { clean: 'saved', dirty: 'unsaved', saving: 'saving', failed: 'failed' }

/* ---------- the title ---------- */

/** Click-to-rename doc title. The server rewrites inbound [[wikilinks]] to
 * the new title in docs the renamer can write (`rename_doc`). */
function DocTitle({ doc, onRenamed, readOnly }: { doc: Doc; onRenamed: () => void; readOnly: boolean }) {
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

  if (readOnly) return <h1 className="dv-title">{doc.title}</h1>
  if (!editing)
    return (
      <h1 className="dv-title">
        <button
          type="button"
          className="dv-title-btn"
          title="Rename"
          onClick={() => {
            setValue(doc.title)
            setEditing(true)
          }}
        >
          {doc.title}
        </button>
      </h1>
    )
  return (
    <input
      className="dv-title dv-title-edit"
      aria-label="Doc title"
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

/* ---------- head pills ---------- */

function StatusPill({ doc, readOnly, onChanged }: { doc: Doc; readOnly: boolean; onChanged: () => void }) {
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLSpanElement>(null)
  useEffect(() => {
    if (!open) return
    const off = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', off)
    return () => document.removeEventListener('mousedown', off)
  }, [open])
  const set = async (status: string | null) => {
    setOpen(false)
    try {
      await api(`/api/doc/${doc.id}/status`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ status }),
      })
      onChanged()
    } catch (e) {
      notify(`status: ${errText(e)}`, 'warn')
    }
  }
  const label = doc.status ?? 'No status'
  if (readOnly)
    return doc.status ? (
      <span className="dv-pill">
        <i className="dv-sdot" style={{ background: statusTone(doc.status) }} aria-hidden />
        {doc.status}
      </span>
    ) : null
  return (
    <span className="dv-menu-wrap" ref={box}>
      <button
        type="button"
        className={`dv-pill${doc.status ? '' : ' dv-pill-dashed'}`}
        aria-label={`Status: ${label}`}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        {doc.status && <i className="dv-sdot" style={{ background: statusTone(doc.status) }} aria-hidden />}
        {doc.status ?? 'Status'}
        <Icon name="chevronDown" size={12} />
      </button>
      {open && (
        <div
          className="dv-menu"
          role="menu"
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.stopPropagation()
              setOpen(false)
            }
          }}
        >
          {STATUSES.map((s, i) => (
            <button key={s} type="button" role="menuitemradio" aria-checked={doc.status === s} autoFocus={i === 0} onClick={() => set(s)}>
              <i className="dv-sdot" style={{ background: statusTone(s) }} aria-hidden />
              {s}
              {doc.status === s && <Icon name="check" size={14} />}
            </button>
          ))}
          {doc.status && (
            <button type="button" role="menuitem" className="dv-menu-clear" onClick={() => set(null)}>
              Clear status
            </button>
          )}
        </div>
      )}
    </span>
  )
}

/** "+ tag": writes the doc's frontmatter (a hidden block) through the gate. */
function AddTag({ onAdd }: { onAdd: (tag: string) => Promise<void> }) {
  const [editing, setEditing] = useState(false)
  const [v, setV] = useState('')
  if (!editing)
    return (
      <button type="button" className="dv-pill dv-pill-dashed" onClick={() => setEditing(true)}>
        + tag
      </button>
    )
  const done = async () => {
    const t = v.trim().replace(/^#/, '')
    setEditing(false)
    setV('')
    if (t) await onAdd(t)
  }
  return (
    <input
      className="dv-pill dv-tag-input"
      autoFocus
      aria-label="New tag"
      placeholder="tag"
      value={v}
      onChange={(e) => setV(e.target.value.replace(/[,\s]/g, '-'))}
      onKeyDown={(e) => {
        if (e.key === 'Enter') done()
        if (e.key === 'Escape') {
          setV('')
          setEditing(false)
        }
      }}
      onBlur={done}
    />
  )
}

/* ---------- the byline ---------- */

function Byline({ rows, epoch, onHistory }: { rows: HistRow[]; epoch: number; onHistory: () => void }) {
  const people = contributors(rows)
  const parts: ReactNode[] = []
  people.forEach((p, i) => {
    if (i > 0 && i === people.length - 1) parts.push(<span key={`j${i}`}>and</span>)
    const comma = i < people.length - 2 ? ',' : ''
    parts.push(
      <span key={p.name} className="dv-by-who">
        <Avatar name={p.name} agent={p.agent} />
        {p.agent ? <span className="lp-agent-name">{p.name}</span> : <span>{p.yours ? 'You' : p.name}</span>}
        {comma && <span className="dv-by-comma">{comma}</span>}
      </span>,
    )
  })
  return (
    <div className="dv-byline">
      {parts}
      {parts.length > 0 && <span aria-hidden>·</span>}
      <span>
        <span className="dv-by-long">version </span>
        <span className="dv-by-short">v</span>
        {epoch}
      </span>
      <span aria-hidden className="dv-by-hist">·</span>
      <button type="button" className="dv-link dv-by-hist" onClick={onHistory}>
        History
      </button>
    </div>
  )
}

/* ---------- the narrow inspector: a sheet ---------- */

function InspectorSheet({ onClose, children }: { onClose: () => void; children: ReactNode }) {
  const box = useRef<HTMLDivElement>(null)
  const trap = useFocusTrap(box)
  return (
    <div className="dv-sheet-scrim" onMouseDown={onClose}>
      <div
        ref={box}
        className="dv-sheet"
        role="dialog"
        aria-modal="true"
        aria-label="Doc inspector"
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation()
            onClose()
            return
          }
          trap(e)
        }}
      >
        <div className="dv-sheet-grab" aria-hidden />
        {children}
      </div>
    </div>
  )
}

/* ---------- the view ---------- */

export default function DocView({
  docId,
  onOpenDoc,
  docs,
  dataVersion,
  anchor,
  reviewIntent = false,
  panel,
  setPanel,
  signedIn = false,
}: {
  docId: string
  onOpenDoc: OpenDoc
  docs: Doc[]
  dataVersion: number
  anchor?: string | null
  /** opened from the review queue: open the Review tab on load */
  reviewIntent?: boolean
  /** owned by App so it outlives this component's per-doc remount */
  panel: Panel
  setPanel: (p: Panel) => void
  /** SERVER mode, signed in through this browser: share links are offered */
  signedIn?: boolean
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
  const fresh = useCallback((g: number) => gen.current === g, [])
  // open review items for THIS doc (yellow = applied+flagged, red = parked)
  const [reviewItems, setReviewItems] = useState<QueueRow[]>([])
  const loadReview = useCallback(() => {
    const g = gen.current
    api<QueueRow[]>(`/api/doc/${docId}/review`)
      .then((r) => fresh(g) && setReviewItems(Array.isArray(r) ? r : []))
      .catch(() => fresh(g) && setReviewItems([]))
  }, [docId, fresh])
  const [history, setHistory] = useState<HistRow[]>([])
  const loadHistory = useCallback(() => {
    const g = gen.current
    api<HistRow[]>(`/api/doc/${docId}/history`)
      .then((r) => fresh(g) && setHistory(Array.isArray(r) ? r : []))
      .catch(() => {})
  }, [docId, fresh])
  const [principals, setPrincipals] = useState<Map<string, PrincipalRow>>(new Map())
  const [me, setMe] = useState<string | null>(null)
  const [workspaces, setWorkspaces] = useState<Workspace[]>([])
  useEffect(() => {
    let live = true
    loadPrincipals().then((m) => live && setPrincipals(m))
    loadMe().then((id) => live && setMe(id))
    api<{ workspaces: Workspace[] }>('/api/workspaces')
      .then((r) => live && setWorkspaces(Array.isArray(r?.workspaces) ? r.workspaces : []))
      .catch(() => {})
    return () => {
      live = false
    }
  }, [dataVersion])

  const [selBlock, setSelBlock] = useState<string | null>(null)
  const [selRect, setSelRect] = useState<{ x: number; y: number } | null>(null)
  const [commentTarget, setCommentTarget] = useState<string | null>(null)
  const [focusThread, setFocusThread] = useState<string | null>(null)
  // epochs this editor produced itself — external changes are anything above
  const ownEpoch = useRef(0)
  const [editorGen, setEditorGen] = useState(0)
  const [saveState, setSaveState] = useState<EditorSaveState>('clean')
  const saveStateRef = useRef<EditorSaveState>('clean')
  saveStateRef.current = saveState

  const wide = useMedia('(min-width: 1280px)')
  const phone = useMedia('(max-width: 760px)')

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
      .then((b) => fresh(g) && setBacklinks(Array.isArray(b) ? b : []))
      .catch(() => fresh(g) && setBacklinks([]))
  }, [docId, fresh])

  useEffect(() => {
    setTree(null)
    // the tab choice is the user's, not the doc's: only a review-queue open
    // overrides it (the same rule as the reviewIntent effect below)
    if (reviewIntent) setPanel('review')
    setCommentTarget(null)
    setReviewItems([])
    setHistory([])
    ownEpoch.current = 0
    loadTree()
    loadReview()
    loadHistory()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadTree, loadReview, loadHistory])

  // a later { review: true } open of the SAME doc still opens Review
  useEffect(() => {
    if (reviewIntent) setPanel('review')
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reviewIntent, anchor])

  useEffect(() => {
    if (dataVersion === 0) return
    loadReview()
    loadHistory()
  }, [dataVersion, loadReview, loadHistory])

  // after a resolve: content may have moved (yellow decline reverts, red
  // accept applies) — refetch, remount the editor if the epoch moved
  const treeRef = useRef<DocTree | null>(null)
  treeRef.current = tree
  const afterResolve = useCallback(() => {
    loadReview()
    loadHistory()
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
  }, [docId, loadReview, loadHistory, fresh])

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
        const dirty = saveStateRef.current !== 'clean'
        if (next.doc.current_epoch > known && !dirty) {
          setTree(next)
          setEditorGen((g) => g + 1)
        } else if (next.doc.status !== cur.doc.status) {
          setTree(next)
        }
        api<SearchHit[]>(`/api/doc/${docId}/backlinks`)
          .then((b) => fresh(g) && setBacklinks(Array.isArray(b) ? b : []))
          .catch(() => {})
      })
      .catch((e) => console.warn('doc refresh failed', docId, e))
  }, [docId, fresh])
  useEffect(() => {
    if (dataVersion === 0) return
    refreshFromStore()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dataVersion])

  // e-readers: the doc's status (the chip, ⌘K's Send / Stop); a sent book's
  // diagrams are drawn and uploaded in the background (once per hash set)
  const ereader = useEreaderStatus(docId)
  useEffect(() => {
    const g = gen.current
    loadEreaderStatus(docId).then((s) => {
      if (fresh(g) && s?.sent) void syncDiagramImages(docId, { status: s })
    })
  }, [docId, dataVersion, fresh])
  // Send / Stop rewrote the frontmatter server-side: show it now, not at the
  // next stamp tick (refreshFromStore leaves a dirty editor alone)
  useEffect(() => {
    const on = (e: Event) => {
      if ((e as CustomEvent<{ docId: string }>).detail?.docId === docId) refreshFromStore()
    }
    window.addEventListener(EREADER_EVENT, on)
    return () => window.removeEventListener(EREADER_EVENT, on)
  }, [docId, refreshFromStore])

  const byTitle = useMemo(() => new Map(docs.map((d) => [d.title, d.id])), [docs])
  const listed = docs.find((d) => d.id === docId)

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
          [...document.querySelectorAll('.ProseMirror h1, .ProseMirror h2, .ProseMirror h3, .ProseMirror h4, .ProseMirror p')].find((n) =>
            n.textContent?.toLowerCase().includes(needle),
          ) ?? null
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

  const { editable, comments, allBlocks, removedCanvases, frontmatter } = useMemo(() => {
    if (!tree)
      return {
        editable: null,
        comments: [] as Block[],
        allBlocks: [] as Block[],
        removedCanvases: 0,
        frontmatter: [] as Block[],
      }
    const { blocks, comments, allBlocks, hidden, removedCanvases } = splitTree(tree.roots)
    return {
      editable: { docId, epoch: tree.doc.current_epoch, blocks, hidden },
      comments,
      allBlocks,
      removedCanvases,
      frontmatter: allBlocks.filter((b) => b.block_type !== 'comment' && b.block_type !== 'canvas_scene' && b.content.startsWith('---')),
    }
  }, [tree, docId])

  // the workspace the doc lives in, and whether this person may write it
  const ws = useMemo(() => docWorkspace(workspaces, docId, listed?.workspace_id), [workspaces, listed, docId])
  const readOnly = ws?.role === 'viewer'

  const who = useCallback(
    (id: string): Principal => {
      const p = principals.get(id)
      const name = p?.display_name ?? 'someone'
      const agent = p ? p.kind !== 'human' : isAgentName(name)
      return { name, agent, yours: !agent && !!me && id === me }
    },
    [principals, me],
  )

  /* ---------- review on the page ---------- */

  const highlightMap = useMemo(() => buildHighlightMap(reviewItems), [reviewItems])
  const commentCounts = useMemo(() => {
    const isComment = new Set(comments.map((c) => c.id))
    const out: Record<string, number> = {}
    for (const c of comments) {
      if (c.parent_id && isComment.has(c.parent_id)) continue
      if (c.refers_to) out[c.refers_to] = (out[c.refers_to] ?? 0) + 1
    }
    return out
  }, [comments])
  const reviewExtras = useMemo<ReviewExtras>(() => {
    const details: Record<string, ReviewDetail> = {}
    for (const it of reviewItems) {
      const id = targetBlockOf(it)
      if (!id) continue
      const tone = toneOf(it)
      if (highlightMap[id] !== tone || details[id]) continue
      const k = it.item.op.kind
      const prior = it.item.op.prior?.content ?? null
      let beforeText: string | null = null
      if (tone === 'yellow' && k.op === 'replace' && prior != null) {
        try {
          beforeText = parser.parse(prior)?.textContent ?? null
        } catch {
          beforeText = null
        }
      }
      const proposed = typeof k.content === 'string' ? k.content : null
      const labels = actionLabels(it)
      details[id] = {
        annotationId: it.item.annotation.id,
        tone,
        who: it.proposer,
        agent: isAgentName(it.proposer),
        confidence: it.item.op.confidence,
        source: it.item.op.source_refs[0] ?? null,
        beforeText,
        proposedText: tone === 'red' && k.op === 'replace' ? proposed : null,
        accept: labels.accept,
        decline: labels.decline,
        canResolve: !readOnly,
      }
    }
    return { details, comments: commentCounts }
  }, [reviewItems, highlightMap, commentCounts, readOnly])

  const [resolving, setResolving] = useState(false)
  const resolveOne = useCallback(
    async (annotationId: string, decision: 'accept' | 'decline') => {
      if (resolving) return
      setResolving(true)
      try {
        await api('/api/resolve', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ annotation_id: annotationId, decision }),
        })
      } catch (e) {
        notify(errText(e))
      }
      setResolving(false)
      afterResolve()
    },
    [afterResolve, resolving],
  )

  /* ---------- the inspector ---------- */

  const auto = panel === 'none'
  const inspectorOpen = panel === 'closed' ? false : auto ? wide : true
  const tab: InspectorTab = auto || panel === 'closed' ? (reviewItems.length > 0 ? 'review' : 'comments') : panel
  const openTab = useCallback((t: InspectorTab) => setPanel(t), [setPanel])
  const closeInspector = useCallback(() => setPanel(wide ? 'closed' : 'none'), [setPanel, wide])

  // clicks inside the doc: wikilinks, a flagged block's Keep / Revert, a
  // comment bubble
  const onStageClick = useCallback(
    (e: React.MouseEvent) => {
      const t = e.target as HTMLElement
      const res = t.closest('[data-resolve]') as HTMLElement | null
      if (res) {
        e.preventDefault()
        const ann = res.dataset.ann
        const d = res.dataset.resolve
        if (ann && (d === 'accept' || d === 'decline')) resolveOne(ann, d)
        return
      }
      const cb = t.closest('[data-comments-for]') as HTMLElement | null
      if (cb) {
        e.preventDefault()
        setFocusThread(cb.dataset.commentsFor ?? null)
        openTab('comments')
        return
      }
      const el = t.closest('.wl-target') as HTMLElement | null
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
    [byTitle, onOpenDoc, resolveOne, openTab],
  )

  /* ---------- tags ---------- */

  const tags = useMemo(() => frontmatterTags(frontmatter.map((b) => b.content)), [frontmatter])
  const addTag = useCallback(
    async (tag: string) => {
      const cur = treeRef.current
      if (!cur) return
      if (tags.includes(tag)) return
      if (saveStateRef.current !== 'clean') {
        notify('wait for the doc to save, then add the tag', 'warn')
        return
      }
      const fm = frontmatter[0] ?? null
      const op = fm
        ? { op: 'replace', target: fm.id, content: withTag(fm.content, tag) }
        : {
            op: 'insert',
            block_id: crypto.randomUUID(),
            parent_id: null,
            order_key: keyBetween(null, cur.roots[0]?.block.order_key ?? null),
            block_type: 'code',
            content: withTag(null, tag),
            refers_to: null,
          }
      try {
        await api('/api/propose', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ doc_id: docId, base_epoch: cur.doc.current_epoch, ops: [{ kind: op, source_refs: [] }] }),
        })
        afterResolve()
      } catch (e) {
        notify(`tag: ${errText(e)}`, 'warn')
      }
    },
    [docId, frontmatter, tags, afterResolve],
  )

  /* ---------- the top bar's slot ---------- */

  const [slot, setSlot] = useState<HTMLElement | null>(null)
  useEffect(() => {
    setSlot(document.getElementById('lp-top-slot'))
  }, [])

  if (!tree || !editable)
    return (
      <div className="dv dv-loading" aria-busy="true">
        <div className="dv-col">
          <div className="dv-skel dv-skel-pills" />
          <div className="dv-skel dv-skel-title" />
          <div className="dv-skel dv-skel-line" />
          <div className="dv-skel dv-skel-line" />
          <div className="dv-skel dv-skel-line short" />
        </div>
      </div>
    )

  const doc = tree.doc
  const saveIndicator: SaveState = readOnly ? 'readonly' : SAVE_OF[saveState]
  const isTended = !!listed?.is_tended

  const inspector = (
    <DocInspector
      tab={tab}
      setTab={openTab}
      onClose={closeInspector}
      closeLabel={wide ? 'Hide inspector' : 'Close'}
      doc={doc}
      reviewItems={reviewItems}
      afterResolve={afterResolve}
      comments={comments}
      allBlocks={allBlocks}
      commentTarget={commentTarget}
      setCommentTarget={setCommentTarget}
      focusThread={focusThread}
      onPosted={loadTree}
      history={history}
      who={who}
      signedIn={signedIn}
      readOnly={readOnly}
      dataVersion={dataVersion}
    />
  )

  const topActions = (
    <>
      <span className="dv-save" role="status" aria-live="polite">
        <StatusDot state={saveIndicator} />
      </span>
      {signedIn && (
        <>
          <Button size="sm" icon="link" className="dv-top-share" onClick={() => openTab('share')}>
            Share
          </Button>
          <IconButton icon="link" label="Share" className="dv-top-share-phone" onClick={() => openTab('share')} />
        </>
      )}
      {!inspectorOpen && !phone && (
        <IconButton icon="comment" label="Show the inspector (review, comments, history)" onClick={() => openTab(tab)} />
      )}
    </>
  )

  return (
    <div className={`dv${inspectorOpen && wide ? ' dv-with-ins' : ''}${readOnly ? ' dv-readonly' : ''}`}>
      {slot && createPortal(topActions, slot)}
      <article className="dv-article" onClick={onStageClick}>
        <div className="dv-col">
          <div className="dv-pills">
            {ws && (
              <button type="button" className="dv-pill" onClick={() => navigate({ kind: 'library', workspace: ws.id })}>
                <i className="dv-wdot" style={{ background: ws.color ?? 'var(--ws-1)' }} aria-hidden />
                {ws.display_name || ws.name}
              </button>
            )}
            <StatusPill doc={doc} readOnly={readOnly} onChanged={refreshFromStore} />
            {tags.map((t) => (
              <button key={t} type="button" className="dv-pill" onClick={() => navigate({ kind: 'search', q: `#${t}` })}>
                #{t}
              </button>
            ))}
            {!readOnly && <AddTag onAdd={addTag} />}
            <LivingChip docId={docId} dataVersion={dataVersion} />
            <EreaderChip docId={docId} title={doc.title} status={ereader} readOnly={readOnly} onOpenDoc={(id) => onOpenDoc(id)} />
            {doc.review_policy && (
              <span className="dv-pill dv-pill-quiet dv-pill-policy" title="who reviews changes to this doc">
                <Icon name="review" size={13} />
                {doc.review_policy}
              </span>
            )}
            {readOnly && (
              <span className="dv-pill dv-pill-quiet">
                <Icon name="members" size={13} />
                Viewer · read only
              </span>
            )}
            <button
              type="button"
              className={`dv-pill dv-pill-quiet${isTended ? ' tended' : ''}`}
              aria-pressed={inspectorOpen && tab === 'tend'}
              onClick={() => openTab('tend')}
              title="agents that look after this doc"
            >
              <Icon name="gardeners" size={13} />
              {isTended ? 'Tended' : 'Tend'}
            </button>
          </div>
          <DocTitle doc={doc} onRenamed={loadTree} readOnly={readOnly} />
          <Byline rows={history} epoch={doc.current_epoch} onHistory={() => openTab('history')} />
          {removedCanvases > 0 && (
            <div className="dv-notice">
              <Icon name="info" size={15} />
              This doc holds {removedCanvases === 1 ? 'a canvas' : `${removedCanvases} canvases`} the app no longer shows.
            </div>
          )}
          <div className="dv-prose">
            <DocEditor
              key={`${docId}:${editorGen}`}
              doc={editable}
              reviewMap={highlightMap}
              reviewExtras={reviewExtras}
              editable={!readOnly}
              onSaveState={setSaveState}
              onSaved={(e, savedDocId) => {
                // the unmount flush of a PREVIOUS doc's editor reports its own id
                if (savedDocId !== docId) return
                ownEpoch.current = Math.max(ownEpoch.current, e)
                loadHistory()
              }}
              onSelectionBlock={onSelectionBlock}
            />
          </div>
          {backlinks.length > 0 && (
            <section className="dv-bl" aria-labelledby="dv-bl-h">
              <h2 id="dv-bl-h">Linked from {backlinks.length}</h2>
              <div className="dv-bl-grid">
                {backlinks.map((b) => (
                  <button key={b.block.id} type="button" className="dv-bl-card" onClick={() => onOpenDoc(b.block.doc_id)}>
                    <b>{b.doc_title}</b>
                    <span>{backlinkSnippet(b.block.content, doc.title)}</span>
                  </button>
                ))}
              </div>
            </section>
          )}
        </div>
      </article>

      {inspectorOpen && wide && (
        <aside className="dv-ins" aria-label="Doc inspector">
          {inspector}
        </aside>
      )}
      {inspectorOpen && !wide && <InspectorSheet onClose={closeInspector}>{inspector}</InspectorSheet>}

      {selBlock && selRect && !readOnly && (
        <span className="dv-sel" style={{ left: selRect.x, top: selRect.y }}>
          <button
            type="button"
            title="Comment on this"
            onMouseDown={(e) => {
              e.preventDefault()
              setCommentTarget(selBlock)
              setFocusThread(selBlock)
              openTab('comments')
              setSelRect(null)
            }}
          >
            <Icon name="comment" size={15} />
            Comment
          </button>
          <button
            type="button"
            title="Copy a link to this block"
            onMouseDown={(e) => {
              e.preventDefault()
              navigator.clipboard
                .writeText(`[[${doc.title}#^${selBlock}]]`)
                .then(() => notify('block link copied', 'ok'))
                .catch(() => {})
              setSelRect(null)
            }}
          >
            <Icon name="link" size={15} />
            Link
          </button>
        </span>
      )}

      <nav className="dv-dock" aria-label="Doc panels">
        <button type="button" onClick={() => openTab('review')} aria-pressed={inspectorOpen && tab === 'review'}>
          <Icon name="review" size={18} />
          Review
          <CountPill n={reviewItems.length} tone="yellow" />
        </button>
        <button type="button" onClick={() => openTab('comments')} aria-pressed={inspectorOpen && tab === 'comments'}>
          <Icon name="comment" size={18} />
          Comments
          <CountPill n={Object.values(commentCounts).reduce((a, b) => a + b, 0)} />
        </button>
        <button type="button" onClick={() => openTab('history')} aria-pressed={inspectorOpen && tab === 'history'}>
          <Icon name="history" size={18} />
          History
        </button>
      </nav>
    </div>
  )
}
