// /library[/<workspace>][?folder=<id>]: the Library (canvas Library). A
// workspace's top level, or one folder's page: the workspace pill and
// "Folder · N docs · edited …", the serif title and lead, New doc here /
// New folder / more; a filter, tag pills, Status and a List / Cards switch;
// then the docs as rows (Title with its first line, Status, Tags, Last
// change by whom, Updated) or as cards. A row with changes waiting says so
// ("2 changes waiting for you", a parked trash in red). Folders open here,
// docs open in the editor.

import { useEffect, useMemo, useRef, useState } from 'react'
import { Avatar, initialOf, Button, Card, EmptyState, Icon, IconButton, Kbd, Spinner, useArm } from '../lapis'
import Link from '../Link'
import { errText, notify } from '../Notice'
import { navigate } from '../router'
import { docPath, scoreDoc } from '../omni'
import { restoreDoc } from '../Trash'
import { childrenIndex, groupRoot, isEmptyFolder, PINNED_TITLES } from '../tree'
import { api, type Doc, type QueueRow } from '../types'
import { docsInWorkspace, workspaceColor, workspaceSummary, type Workspace, type WorkspaceSel } from '../workspaces'
import ChipMenu from './ChipMenu'
import { excerptOf, loadLastChange, loadTree, useDocTags, type LastChange } from './docCache'
import { shortWhen } from './searchText'
import './library.css'

type Status = NonNullable<Doc['status']>
const STATUS_WORD: Record<Status, string> = { decided: 'decided', 'in-review': 'in review', draft: 'draft', superseded: 'superseded' }
type StatusFilter = 'any' | Status | 'none'
type View = 'list' | 'cards'

const VIEW_KEY = 'taisce.library.view'
function loadView(): View {
  try {
    return localStorage.getItem(VIEW_KEY) === 'cards' ? 'cards' : 'list'
  } catch {
    return 'list'
  }
}
function storeView(v: View) {
  try {
    localStorage.setItem(VIEW_KEY, v)
  } catch {
    /* private window: the choice lasts this visit */
  }
}

/** Per doc: changes waiting for review, and a parked change (with whether it
 * is a trash/delete, and who proposed it). */
export interface PendingInfo {
  review: number
  parked: number
  parkedTrash: string | null
}

export function pendingByDoc(rows: QueueRow[]): Map<string, PendingInfo> {
  const m = new Map<string, PendingInfo>()
  for (const r of rows) {
    const id = r.item.annotation.doc_id
    const cur = m.get(id) ?? { review: 0, parked: 0, parkedTrash: null }
    if (r.item.annotation.kind === 'parked' || r.item.op.verdict === 'red') {
      cur.parked += 1
      const op = String(r.item.op.kind?.op ?? '')
      if (op === 'delete_doc') cur.parkedTrash = r.proposer
    } else cur.review += 1
    m.set(id, cur)
  }
  return m
}

interface RowInfo {
  excerpt?: string | null
  last?: LastChange | null
}

/** Lazily load each shown doc's first line and last change (cached per
 * epoch in docCache, so a refetch only asks about what changed). */
function useRowInfo(rows: Doc[]): Map<string, RowInfo> {
  const [info, setInfo] = useState<Map<string, RowInfo>>(new Map())
  const key = rows.map((d) => `${d.id}@${d.current_epoch}`).join(',')
  useEffect(() => {
    let live = true
    const take = rows.slice(0, 80)
    Promise.all(
      take.map(async (d) => {
        const [tree, last] = await Promise.all([d.current_epoch > 0 ? loadTree(d.id, d.current_epoch) : Promise.resolve(null), loadLastChange(d.id, d.current_epoch)])
        return [d.id, { excerpt: tree ? excerptOf(tree.roots) : null, last }] as const
      }),
    ).then((pairs) => {
      if (live) setInfo(new Map(pairs))
    })
    return () => {
      live = false
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key])
  return info
}

export default function Library({
  docs: allDocs,
  workspace,
  folder = null,
  workspaces,
  queue = [],
  dataVersion = 0,
  userName = null,
  onChanged,
}: {
  docs: Doc[]
  workspace: WorkspaceSel
  /** a folder's page, or the workspace's top level */
  folder?: string | null
  workspaces: Workspace[]
  /** the review queue (App's), for "changes waiting" lines */
  queue?: QueueRow[]
  dataVersion?: number
  /** your name (the "You" avatar's initial) */
  userName?: string | null
  /** the doc list changed here (a new doc or folder, a trash) */
  onChanged: () => void
}) {
  const byId = useMemo(() => new Map(allDocs.map((d) => [d.id, d])), [allDocs])
  const folderDoc = folder ? byId.get(folder) : undefined
  // a folder's page shows the folder wherever it lives; the top level is
  // the switcher's workspace
  const docs = useMemo(() => (folderDoc ? allDocs : docsInWorkspace(allDocs, workspace)), [allDocs, workspace, folderDoc])
  const kids = useMemo(() => childrenIndex(docs), [docs])
  const pending = useMemo(() => pendingByDoc(queue), [queue])
  const docTags = useDocTags(dataVersion)

  const [filter, setFilter] = useState('')
  const [tag, setTag] = useState<string | null>(null)
  const [status, setStatus] = useState<StatusFilter>('any')
  const [view, setViewRaw] = useState<View>(loadView)
  const setView = (v: View) => {
    setViewRaw(v)
    storeView(v)
  }
  // leaving for another folder clears the filters
  useEffect(() => {
    setFilter('')
    setTag(null)
    setStatus('any')
  }, [folder, workspace])

  /* ---- what this page lists ---- */
  const here: Doc[] = useMemo(() => {
    if (folder) return kids.get(folder) ?? []
    const g = groupRoot(docs, kids)
    return [...g.pinned, ...g.folders, ...g.notes]
  }, [folder, kids, docs])
  const subtree: Doc[] = useMemo(() => {
    const out: Doc[] = []
    const walk = (list: Doc[]) => {
      for (const d of list) {
        out.push(d)
        walk(kids.get(d.id) ?? [])
      }
    }
    walk(here)
    return out
  }, [here, kids])
  const isFolder = (d: Doc) => (kids.get(d.id)?.length ?? 0) > 0 || isEmptyFolder(d)

  const rows = useMemo(() => {
    const q = filter.trim()
    let list = q
      ? subtree
          .map((d) => ({ d, s: scoreDoc(q, d.title, docPath(d, byId)) }))
          .filter((r) => r.s !== null)
          .sort((a, b) => (b.s ?? 0) - (a.s ?? 0))
          .map((r) => r.d)
      : here
    if (tag) list = list.filter((d) => (docTags.get(d.id) ?? []).includes(tag))
    if (status !== 'any') list = list.filter((d) => (status === 'none' ? !d.status : d.status === status))
    return list
  }, [filter, subtree, here, byId, tag, docTags, status])

  const info = useRowInfo(rows)
  const tagsHere = useMemo(() => {
    const count = new Map<string, number>()
    for (const d of subtree) for (const t of docTags.get(d.id) ?? []) count.set(t, (count.get(t) ?? 0) + 1)
    const top = [...count.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([t]) => t)
    return top.slice(0, 5)
  }, [subtree, docTags])

  /* ---- the head ---- */
  const wsId = folderDoc ? (folderDoc.workspace_id ?? 'unsorted') : workspace
  const wi = workspaces.findIndex((w) => w.id === wsId)
  const { name: wsName } = workspaceSummary(wsId, workspaces, 0)
  const wsDot = wi >= 0 ? workspaceColor(workspaces[wi], wi) : 'var(--line2)'
  const [lead, setLead] = useState<string | null>(null)
  useEffect(() => {
    setLead(null)
    if (!folderDoc || folderDoc.current_epoch === 0) return
    let live = true
    loadTree(folderDoc.id, folderDoc.current_epoch).then((t) => live && setLead(t ? excerptOf(t.roots, 240) : null))
    return () => {
      live = false
    }
  }, [folderDoc])
  const lastEdit = useMemo(() => {
    let best: number | null = null
    for (const d of here) {
      const at = info.get(d.id)?.last?.at
      if (at && (!best || at > best)) best = at
    }
    return best
  }, [here, info])
  const nDocs = subtree.length
  const title = folderDoc ? folderDoc.title : wsName

  /* ---- create ---- */
  const [creating, setCreating] = useState<null | 'doc' | 'folder'>(null)
  const [newTitle, setNewTitle] = useState('')
  const [busy, setBusy] = useState(false)
  const newRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (creating) newRef.current?.focus()
  }, [creating])
  const create = async () => {
    const t = newTitle.trim()
    if (!t || busy) return
    setBusy(true)
    try {
      const body: Record<string, unknown> = { title: t, parent_doc_id: folder ?? null }
      // a new top-level doc lands in the workspace this page shows
      if (!folder && workspace !== 'all' && workspace !== 'unsorted') body.workspace_id = workspace
      const d = await api<Doc>('/api/docs', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
      setNewTitle('')
      const kind = creating
      setCreating(null)
      onChanged()
      if (kind === 'doc') navigate({ kind: 'doc', id: d.id })
      else notify(`made the folder “${t}”`, 'ok', { onClick: () => navigate({ kind: 'library', workspace: workspace === 'all' ? null : workspace, folder: d.id }) })
    } catch (e) {
      notify(errText(e))
    } finally {
      setBusy(false)
    }
  }

  /* ---- more: open the folder doc, trash it ---- */
  const [moreOpen, setMoreOpen] = useState(false)
  const moreBox = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!moreOpen) return
    const off = (e: MouseEvent) => {
      if (!moreBox.current?.contains(e.target as Node)) setMoreOpen(false)
    }
    window.addEventListener('mousedown', off)
    return () => window.removeEventListener('mousedown', off)
  }, [moreOpen])
  const { armed, press } = useArm<string>()
  const trashFolder = async () => {
    if (!folderDoc || !press(folderDoc.id)) return
    setMoreOpen(false)
    try {
      const out = await api<{ deleted: number }>(`/api/doc/${folderDoc.id}/delete`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' })
      const inside = out.deleted > 1 ? ` and ${out.deleted - 1} inside it` : ''
      const back = folderDoc
      notify(`moved “${back.title}”${inside} to the Trash — click to undo`, 'ok', {
        ttlMs: 10_000,
        onClick: () =>
          restoreDoc(back.id)
            .then(() => {
              notify(`restored “${back.title}”`, 'ok')
              onChanged()
            })
            .catch((e) => notify(errText(e))),
      })
      onChanged()
      navigate({ kind: 'library', workspace: workspace === 'all' ? null : workspace, folder: back.parent_id ?? undefined }, { replace: true })
    } catch (e) {
      notify(errText(e))
    }
  }

  if (folder && !folderDoc) {
    return (
      <div className="lp-page lp-lib">
        <Card>
          <EmptyState icon="folder" title="That folder is not here" action={<Link to={{ kind: 'library', workspace: null }}>Back to the Library</Link>}>
            It may have been moved to the Trash.
          </EmptyState>
        </Card>
      </div>
    )
  }

  const wsParam = workspace === 'all' ? null : workspace
  const toRow = (d: Doc) => (isFolder(d) ? { kind: 'library' as const, workspace: wsParam, folder: d.id } : { kind: 'doc' as const, id: d.id })

  const second = (d: Doc) => {
    const p = pending.get(d.id)
    if (p?.parkedTrash) return <span className="lp-lib-sub red">{p.parkedTrash} wants to trash this · parked</span>
    if (p && p.parked > 0) return <span className="lp-lib-sub red">{p.parked} parked change{p.parked === 1 ? '' : 's'} · not applied</span>
    if (p && p.review > 0) return <span className="lp-lib-sub yellow">{p.review} change{p.review === 1 ? '' : 's'} waiting for you</span>
    if (isFolder(d)) {
      const n = kids.get(d.id)?.length ?? 0
      return <span className="lp-lib-sub">{n === 0 ? 'Empty folder' : `${n} doc${n === 1 ? '' : 's'}`}</span>
    }
    const ex = info.get(d.id)?.excerpt
    if (filter.trim()) {
      const parent = docPath(d, byId).split(' › ').slice(0, -1).join(' › ')
      if (parent) return <span className="lp-lib-sub">{parent}</span>
    }
    return ex ? <span className="lp-lib-sub">{ex}</span> : null
  }
  const statusCell = (d: Doc) =>
    d.status ? <span className={`lp-st lp-st-${d.status}`}>{STATUS_WORD[d.status]}</span> : <span className="muted">—</span>
  const tagsCell = (d: Doc) => (
    <span className="lp-lib-tags">
      {(docTags.get(d.id) ?? []).slice(0, 3).map((t) => (
        <span key={t} className="lp-tag">
          {t}
        </span>
      ))}
    </span>
  )
  const whoCell = (d: Doc) => {
    const last = info.get(d.id)?.last
    if (last === undefined) return <span className="muted">…</span>
    if (!last) return <span className="muted">—</span>
    if (!last.agent && last.who === 'You')
      return (
        <span className="lp-who">
          <span className="lp-avatar lp-avatar-person" style={{ width: 22, height: 22, fontSize: 11 }} aria-hidden>
            {initialOf(userName || 'You')}
          </span>
          <span className="lp-person-name">You</span>
        </span>
      )
    return <Avatar name={last.who} agent={last.agent} size={22} showName />
  }
  const whenCell = (d: Doc) => {
    const at = info.get(d.id)?.last?.at
    return <span className="muted lp-lib-when">{at ? shortWhen(at) : ''}</span>
  }

  const onListKey = (e: React.KeyboardEvent) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
    const els = Array.from((e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>('a.lp-lib-row, a.lp-lib-card'))
    const i = els.indexOf(document.activeElement as HTMLElement)
    if (i < 0) return
    e.preventDefault()
    els[Math.max(0, Math.min(els.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))]?.focus()
  }

  return (
    <div className="lp-page lp-lib">
      <header className="lp-page-head">
        <div className="lp-page-head-text">
          <div className="lp-page-kicker">
            <span className="lp-pill">
              <span className="lp-dot" style={{ background: wsDot }} aria-hidden />
              {wsName}
            </span>
            <span className="muted">
              {folderDoc ? 'Folder' : 'Library'} · {nDocs} doc{nDocs === 1 ? '' : 's'}
              {lastEdit ? ` · edited ${shortWhen(lastEdit)}${/^\d+ min$/.test(shortWhen(lastEdit)) ? ' ago' : ''}` : ''}
            </span>
          </div>
          <h1 className="lp-page-title">{title}</h1>
          {folderDoc ? lead && <p className="lp-page-lead">{lead}</p> : <p className="lp-page-lead">Folders first, then loose docs. Open a folder to see what is inside it.</p>}
        </div>
        <div className="lp-page-actions">
          <Button size="sm" icon="docPlus" onClick={() => setCreating(creating === 'doc' ? null : 'doc')} aria-expanded={creating === 'doc'}>
            New doc here
          </Button>
          <Button size="sm" icon="folder" onClick={() => setCreating(creating === 'folder' ? null : 'folder')} aria-expanded={creating === 'folder'}>
            New folder
          </Button>
          {folderDoc && (
            <div className="lp-lib-more" ref={moreBox}>
              <IconButton icon="more" label="More folder actions" size="md" aria-haspopup="menu" aria-expanded={moreOpen} onClick={() => setMoreOpen((o) => !o)} />
              {moreOpen && (
                <div className="lp-chipmenu-list lp-lib-moremenu" role="menu" onKeyDown={(e) => e.key === 'Escape' && setMoreOpen(false)}>
                  <button type="button" role="menuitem" onClick={() => navigate({ kind: 'doc', id: folderDoc.id })}>
                    <Icon name="doc" size={15} />
                    <span>Open “{folderDoc.title}” as a doc</span>
                  </button>
                  {!PINNED_TITLES.has(folderDoc.title) && (
                    <button type="button" role="menuitem" className="danger" onClick={trashFolder}>
                      <Icon name="trash" size={15} />
                      <span>{armed === folderDoc.id ? 'Click again to move it to the Trash' : 'Move to Trash…'}</span>
                    </button>
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      </header>

      {creating && (
        <form
          className="lp-card lp-lib-new"
          onSubmit={(e) => {
            e.preventDefault()
            create()
          }}
        >
          <Icon name={creating === 'doc' ? 'docPlus' : 'folder'} size={16} />
          <label htmlFor="lp-lib-new" className="lp-lib-sr">
            {creating === 'doc' ? 'New doc title' : 'New folder name'}
          </label>
          <input
            id="lp-lib-new"
            ref={newRef}
            value={newTitle}
            placeholder={creating === 'doc' ? `New doc in ${title}…` : `New folder in ${title}…`}
            onChange={(e) => setNewTitle(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                e.stopPropagation()
                setCreating(null)
              }
            }}
          />
          {busy && <Spinner size={14} />}
          <Button type="submit" variant="primary" size="sm" disabled={!newTitle.trim() || busy}>
            {creating === 'doc' ? 'Create and open' : 'Make folder'}
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setCreating(null)}>
            Cancel
          </Button>
        </form>
      )}

      <div className="lp-lib-tools">
        <label htmlFor="lp-lib-flt" className="lp-lib-sr">
          Filter this {folderDoc ? 'folder' : 'library'}
        </label>
        <input
          id="lp-lib-flt"
          className="lp-lib-filter"
          placeholder={`Filter this ${folderDoc ? 'folder' : 'library'}`}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Escape' && filter) {
              e.stopPropagation()
              setFilter('')
            }
          }}
        />
        {tagsHere.map((t) => (
          <button key={t} type="button" className={`lp-fchip lp-lib-pill${tag === t ? ' on' : ''}`} aria-pressed={tag === t} onClick={() => setTag(tag === t ? null : t)}>
            #{t}
            {tag === t && <Icon name="close" size={12} />}
          </button>
        ))}
        <ChipMenu<StatusFilter>
          label="Status"
          value={status}
          options={[
            { id: 'any', label: 'any' },
            { id: 'draft', label: 'draft' },
            { id: 'in-review', label: 'in review' },
            { id: 'decided', label: 'decided' },
            { id: 'superseded', label: 'superseded' },
            { id: 'none', label: 'none set' },
          ]}
          onChange={setStatus}
        />
        <div className="lp-lib-view" role="group" aria-label="View">
          <button type="button" aria-pressed={view === 'list'} className={view === 'list' ? 'on' : ''} onClick={() => setView('list')}>
            List
          </button>
          <button type="button" aria-pressed={view === 'cards'} className={view === 'cards' ? 'on' : ''} onClick={() => setView('cards')}>
            Cards
          </button>
        </div>
      </div>

      {rows.length === 0 ? (
        <Card>
          <EmptyState
            icon="folder"
            title={filter || tag || status !== 'any' ? 'Nothing matches' : folderDoc ? 'This folder is empty' : 'No docs here yet'}
            compact
            action={
              !filter && !tag && status === 'any' ? (
                <Button size="sm" variant="primary" icon="docPlus" onClick={() => setCreating('doc')}>
                  New doc here
                </Button>
              ) : undefined
            }
          >
            {filter || tag || status !== 'any' ? 'Try fewer letters, or clear a filter.' : 'Docs you make or move here show up in this list.'}
          </EmptyState>
        </Card>
      ) : view === 'list' ? (
        <section className="lp-card lp-card-flush lp-lib-table" aria-label={`Docs in ${title}`} onKeyDown={onListKey}>
          <div className="lp-lib-row lp-lib-rowh" aria-hidden>
            <span>Title</span>
            <span>Status</span>
            <span>Tags</span>
            <span>Last change</span>
            <span className="lp-lib-when">Updated</span>
          </div>
          {rows.map((d) => (
            <Link key={d.id} to={toRow(d)} className="lp-lib-row">
              <span className="lp-lib-title">
                <Icon name={isFolder(d) ? 'folder' : 'doc'} size={15} />
                <span className="lp-lib-titletext">
                  <b>{d.title || 'Untitled'}</b>
                  {second(d)}
                </span>
              </span>
              <span className="lp-lib-c-status">{statusCell(d)}</span>
              <span className="lp-lib-c-tags">{tagsCell(d)}</span>
              <span className="lp-lib-c-who">{whoCell(d)}</span>
              {whenCell(d)}
            </Link>
          ))}
        </section>
      ) : (
        <section className="lp-lib-cards" aria-label={`Docs in ${title}`} onKeyDown={onListKey}>
          {rows.map((d) => (
            <Link key={d.id} to={toRow(d)} className="lp-card lp-lib-card">
              <span className="lp-lib-card-top">
                <Icon name={isFolder(d) ? 'folder' : 'doc'} size={16} />
                {statusCell(d)}
                {whenCell(d)}
              </span>
              <b className="lp-lib-card-title">{d.title || 'Untitled'}</b>
              {second(d)}
              <span className="lp-lib-card-foot">
                {whoCell(d)}
                {tagsCell(d)}
              </span>
            </Link>
          ))}
        </section>
      )}

      <div className="lp-lib-hints muted">
        <span>
          <Kbd>↵</Kbd> open
        </span>
        <span>
          <Kbd>↑</Kbd>
          <Kbd>↓</Kbd> move between rows
        </span>
        <span>Folders open here, docs in the editor</span>
        <span>Drag docs in the sidebar tree to move them</span>
      </div>
    </div>
  )
}
