// The file tree (⌘T): a quiet library. Typography carries the hierarchy —
// folders in --fg at weight 500, leaves in --fg-dim, a faint guide per
// depth — and the root is grouped into Pinned / Folders / Notes. Behaviour
// is unchanged from the old DocTreeNav: drag into/before/after, the
// shared-subtree move confirm, arm-then-confirm delete with an undo toast.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { api, Doc } from './types'
import { notify, errText } from './Notice'
import { restoreDoc } from './Trash'
import { keyForPosition } from './editor/diff'
import {
  Section,
  ancestorsOf,
  childrenIndex,
  descendantCount,
  filterDocs,
  groupRoot,
  loadTreeState,
  saveTreeState,
} from './tree'

type Drop = { id: string; mode: 'into' | 'before' | 'after' } | null

const SECTION_LABEL: Record<Section, string> = { pinned: 'Pinned', folders: 'Folders', notes: 'Notes' }

/** Expand/collapse with a 120ms grid-rows transition; children stay mounted
 * only while open or animating shut, so a collapsed vault is not all in the
 * DOM. */
function Collapse({ open, children }: { open: boolean; children: React.ReactNode }) {
  const [mounted, setMounted] = useState(open)
  useEffect(() => {
    if (open) {
      setMounted(true)
      return
    }
    const t = setTimeout(() => setMounted(false), 140)
    return () => clearTimeout(t)
  }, [open])
  if (!mounted) return null
  return (
    <div className={`tree-collapse ${open ? 'open' : ''}`}>
      <div className="tree-collapse-inner">{children}</div>
    </div>
  )
}

export default function DocTree({
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
  const childrenOf = useMemo(() => childrenIndex(docs), [docs])
  const byId = useMemo(() => new Map(docs.map((d) => [d.id, d])), [docs])
  const groups = useMemo(() => groupRoot(docs, childrenOf), [docs, childrenOf])

  /* ---- open state, persisted (try/catch inside load/save) ---- */
  const persisted = useRef(loadTreeState())
  const [openDirs, setOpenDirs] = useState<Set<string>>(() => new Set(persisted.current.open))
  const [closedSections, setClosedSections] = useState<Set<Section>>(
    () => new Set(persisted.current.closedSections),
  )
  useEffect(() => {
    saveTreeState({ closedSections: [...closedSections], open: [...openDirs] })
  }, [openDirs, closedSections])
  const toggleDir = useCallback((id: string) => {
    setOpenDirs((s) => {
      const n = new Set(s)
      if (n.has(id)) n.delete(id)
      else n.add(id)
      return n
    })
  }, [])
  const toggleSection = (s: Section) =>
    setClosedSections((cur) => {
      const n = new Set(cur)
      if (n.has(s)) n.delete(s)
      else n.add(s)
      return n
    })

  // the open doc is always reachable: expand its ancestors
  useEffect(() => {
    if (!selected) return
    const anc = ancestorsOf(selected, byId)
    if (anc.length === 0) return
    setOpenDirs((s) => {
      if (anc.every((a) => s.has(a))) return s
      const n = new Set(s)
      for (const a of anc) n.add(a)
      return n
    })
  }, [selected, byId])

  /* ---- filter ---- */
  const [filter, setFilter] = useState('')
  const filterRef = useRef<HTMLInputElement>(null)
  const filtered = useMemo(() => filterDocs(docs, filter, byId), [docs, filter, byId])
  const isOpen = (id: string) => (filtered ? filtered.expanded.has(id) || openDirs.has(id) : openDirs.has(id))
  const isVisible = (id: string) => !filtered || filtered.visible.has(id)

  /* ---- drag & drop ---- */
  const [dragging, setDragging] = useState<string | null>(null)
  const [drop, setDrop] = useState<Drop>(null)
  const [pendingMove, setPendingMove] = useState<{
    dragged: string
    parent: string | null
    sortKey: string | null
    sharedRoot: string
  } | null>(null)

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

  /* ---- delete: arm-then-confirm (window.confirm is a no-op in Tauri) ---- */
  const [armed, setArmed] = useState<string | null>(null)
  const armTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(
    () => () => {
      if (armTimer.current) clearTimeout(armTimer.current)
    },
    [],
  )
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

  /* ---- rows ---- */
  const renderRow = (d: Doc, depth: number, rootSection: Section | null): React.ReactNode => {
    if (!isVisible(d.id)) return null
    const kids = childrenOf.get(d.id) ?? []
    const isDir = kids.length > 0
    const open = isDir && isOpen(d.id)
    const dropHere = drop?.id === d.id ? drop.mode : null
    const isSel = selected === d.id
    // root Folders/Pinned are alphabetical: before/after would not show, so
    // a drop there is always "into"
    const intoOnly = rootSection === 'folders' || rootSection === 'pinned'
    const match = filtered?.matches.has(d.id)
    return (
      <div key={d.id} className="tree-node">
        <div
          className={[
            'tree-item',
            isDir ? 'dir' : 'leaf',
            isSel ? 'sel' : '',
            match ? 'match' : '',
            dragging === d.id ? 'dragging' : '',
            dropHere === 'into' ? 'drop-into' : '',
            dropHere === 'before' ? 'drop-before' : '',
            dropHere === 'after' ? 'drop-after' : '',
          ]
            .filter(Boolean)
            .join(' ')}
          style={{ paddingLeft: 10 + depth * 14 }}
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
            const mode = intoOnly ? 'into' : y < 0.3 ? 'before' : y > 0.7 ? 'after' : 'into'
            setDrop({ id: d.id, mode })
          }}
          onDragLeave={() => setDrop((cur) => (cur?.id === d.id ? null : cur))}
          onDrop={(e) => {
            e.preventDefault()
            if (dragging && drop?.id === d.id) doMove(dragging, d, drop.mode)
            setDrop(null)
            setDragging(null)
          }}
          onClick={() => {
            if (isDir) toggleDir(d.id)
            onSelect(d.id)
          }}
          title={d.title}
        >
          {isDir ? (
            <svg width="10" height="10" viewBox="0 0 10 10" className={open ? 'chev open' : 'chev'} aria-hidden>
              <path d="M3 1.5 L7 5 L3 8.5" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
            </svg>
          ) : (
            <span className="tree-leaf-slot" aria-hidden />
          )}
          {d.is_canvas && <span className="tree-canvas" title="canvas">▨</span>}
          <span className="tree-title">{d.title}</span>
          <span className="tree-badges">
            {isDir && !open && <span className="tree-count">{descendantCount(d.id, childrenOf)}</span>}
            {d.is_tended && <span className="tend-dot" title="tended by agents" />}
            {d.mirror_permission && !d.from_hub && (
              <span className="mirror-badge quiet" title={`shared with you (${d.mirror_permission})`}>⇄</span>
            )}
            {d.from_hub && (
              <span
                className="mirror-badge hub quiet"
                title={d.origin_owner_name ? `relayed by the hub · owned by ${d.origin_owner_name}` : 'a hub folder'}
              >
                ⌂
              </span>
            )}
            {d.is_shared && !d.published_to && (
              <span className="shared-badge quiet" title="you share this subtree">↗</span>
            )}
            {d.published_to && !d.mirror_permission && (
              <span className="shared-badge hub quiet" title={`published to ${d.published_to}`}>⌂</span>
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
          </span>
        </div>
        {isDir && (
          <Collapse open={open}>
            <div
              className="tree-children"
              style={{ ['--guide' as string]: `${10 + depth * 14 + 5}px` } as React.CSSProperties}
            >
              {kids.map((k) => renderRow(k, depth + 1, null))}
            </div>
          </Collapse>
        )}
      </div>
    )
  }

  const renderSection = (s: Section, rows: Doc[]) => {
    if (rows.length === 0) return null
    const shown = rows.filter((d) => isVisible(d.id))
    if (filtered && shown.length === 0) return null
    const open = filtered ? true : !closedSections.has(s)
    return (
      <div key={s} className="tree-section">
        <button className={`tree-section-head ${open ? 'open' : ''}`} onClick={() => toggleSection(s)}>
          <svg width="8" height="8" viewBox="0 0 10 10" className={open ? 'chev open' : 'chev'} aria-hidden>
            <path d="M3 1.5 L7 5 L3 8.5" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
          </svg>
          {SECTION_LABEL[s]}
          {!open && <span className="tree-count">{rows.length}</span>}
        </button>
        <Collapse open={open}>{shown.map((d) => renderRow(d, 0, s))}</Collapse>
      </div>
    )
  }

  return (
    <aside className="sidebar">
      <div className="sidebar-head">
        <input
          ref={filterRef}
          className="tree-filter"
          placeholder="filter · ⌘T to close"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== 'Escape') return
            // first Esc clears the filter, the second closes the tree; the
            // global Esc (palettes) must not see either
            e.stopPropagation()
            e.preventDefault()
            if (filter) setFilter('')
            else onClose()
          }}
          spellCheck={false}
        />
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
        {renderSection('pinned', groups.pinned)}
        {renderSection('folders', groups.folders)}
        {renderSection('notes', groups.notes)}
        {filtered && filtered.matches.size === 0 && <div className="tree-empty">no titles match</div>}
      </div>
    </aside>
  )
}
