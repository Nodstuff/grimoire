// The doc tree in the Lapis sidebar (shell/Sidebar.tsx): a Pinned group
// (the system folders), then the Library (folders, then loose docs), shown
// for the workspace the sidebar's switcher picked. Behaviour is unchanged
// from the old ⌘T tree: drag into/before/after, drop on empty space = top
// level, arm-then-confirm delete with an undo toast, a title filter.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { api, Doc } from './types'
import { Icon, IconButton } from './lapis'
import Link from './Link'
import { WorkspaceSel, docsInWorkspace } from './workspaces'
import { notify, errText } from './Notice'
import { restoreDoc } from './Trash'
import { keyForPosition } from './editor/diff'
import {
  Section,
  ancestorsOf,
  childrenIndex,
  descendantCounts,
  filterDocs,
  groupRoot,
  loadTreeState,
  saveTreeState,
  isEmptyFolder,
} from './tree'

type Drop = { id: string; mode: 'into' | 'before' | 'after' } | null

const SECTION_LABEL: Record<Section, string> = { pinned: 'Pinned', folders: 'Library', notes: 'Library' }

/** Expand/collapse with a 120ms grid-rows transition; children stay mounted
 * only while open or animating shut, so a collapsed vault is not all in the
 * DOM. */
function Collapse({ open, children }: { open: boolean; children: React.ReactNode }) {
  const [mounted, setMounted] = useState(open)
  // the `open` class, one frame behind `mounted`. Mounting the row and adding
  // the class in the same commit gives the browser no 0fr start value to
  // transition FROM, so the first expand snaps instead of growing; two rAFs
  // put the class after the paint that established the collapsed height.
  // (Reasoned, not yet watched in a browser.)
  const [grown, setGrown] = useState(open)
  useEffect(() => {
    if (open) {
      setMounted(true)
      let inner = 0
      const outer = requestAnimationFrame(() => {
        inner = requestAnimationFrame(() => setGrown(true))
      })
      return () => {
        cancelAnimationFrame(outer)
        cancelAnimationFrame(inner)
      }
    }
    setGrown(false)
    const t = setTimeout(() => setMounted(false), 140)
    return () => clearTimeout(t)
  }, [open])
  if (!mounted) return null
  return (
    <div className={`tree-collapse ${grown ? 'open' : ''}`}>
      <div className="tree-collapse-inner">{children}</div>
    </div>
  )
}

export default function DocTree({
  docs: allDocs,
  selected,
  onSelect,
  onChanged,
  workspace,
  wsColor,
  onNewDoc,
  focusFilter = 0,
}: {
  docs: Doc[]
  selected: string | null
  onSelect: (id: string) => void
  onChanged: () => void
  /** which workspace the tree shows (the sidebar's switcher) */
  workspace: WorkspaceSel
  /** workspace id → colour, for the dot on top-level rows under All */
  wsColor?: Map<string, string>
  /** the + beside the Library label */
  onNewDoc?: () => void
  /** bump to put the caret in the filter (⌘T opening the sidebar) */
  focusFilter?: number
}) {
  const docs = useMemo(() => docsInWorkspace(allDocs, workspace), [allDocs, workspace])
  const childrenOf = useMemo(() => childrenIndex(docs), [docs])
  const byId = useMemo(() => new Map(docs.map((d) => [d.id, d])), [docs])
  const groups = useMemo(() => groupRoot(docs, childrenOf), [docs, childrenOf])
  const counts = useMemo(() => descendantCounts(childrenOf), [childrenOf])

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
  // ⌘T opened the sidebar: put the caret in the filter so typing follows
  useEffect(() => {
    if (focusFilter) filterRef.current?.focus()
  }, [focusFilter])
  const filtered = useMemo(() => filterDocs(docs, filter, byId), [docs, filter, byId])
  const isOpen = (id: string) => (filtered ? filtered.expanded.has(id) || openDirs.has(id) : openDirs.has(id))
  const isVisible = (id: string) => !filtered || filtered.visible.has(id)

  /* ---- drag & drop ---- */
  const [dragging, setDragging] = useState<string | null>(null)
  const [drop, setDrop] = useState<Drop>(null)
  // the actual move; the server may refuse (a cycle) — its error string
  // surfaces as a notice
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
    const isDir = kids.length > 0 || isEmptyFolder(d)
    const open = isDir && isOpen(d.id)
    const dropHere = drop?.id === d.id ? drop.mode : null
    const isSel = selected === d.id
    // root Folders/Pinned are alphabetical: before/after would not show, so
    // a drop there is always "into"
    const intoOnly = rootSection === 'folders' || rootSection === 'pinned'
    const match = filtered?.matches.has(d.id)
    const dot = depth === 0 && workspace === 'all' && d.workspace_id ? wsColor?.get(d.workspace_id) : undefined
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
          style={{ paddingLeft: 10 + depth * 24 }}
          draggable
          role="treeitem"
          aria-selected={isSel}
          aria-expanded={isDir ? open : undefined}
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.target !== e.currentTarget) return
            if (e.key === 'Enter') {
              e.preventDefault()
              if (isDir) toggleDir(d.id)
              onSelect(d.id)
            } else if (isDir && (e.key === 'ArrowRight' || e.key === 'ArrowLeft')) {
              e.preventDefault()
              if ((e.key === 'ArrowRight') !== open) toggleDir(d.id)
            }
          }}
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
          {rootSection === 'pinned' ? (
            <Icon name={isDir ? 'folder' : 'doc'} size={18} />
          ) : isDir ? (
            <Icon name={open ? 'chevronDown' : 'chevronRight'} size={15} className="chev" />
          ) : depth === 0 ? (
            <Icon name="doc" size={15} className="tree-doc-icon" />
          ) : null}
          {dot && <span className="lp-dot" style={{ background: dot }} aria-hidden />}
          <span className="tree-title">{d.title}</span>
          <span className="tree-badges">
            {d.is_tended && <span className="tend-dot" title="tended by agents" />}
            {isDir && !open && <span className="tree-count">{counts.get(d.id) ?? 0}</span>}
            <button
              type="button"
              className={`tree-delete ${armed === d.id ? 'armed' : ''}`}
              title={armed === d.id ? 'click again to move to Trash' : 'move to Trash'}
              aria-label={armed === d.id ? `Confirm: move ${d.title} to Trash` : `Move ${d.title} to Trash`}
              onClick={(e) => {
                e.stopPropagation()
                doDelete(d)
              }}
            >
              {armed === d.id ? 'Sure?' : <Icon name="close" size={13} />}
            </button>
          </span>
        </div>
        {isDir && (
          <Collapse open={open}>
            <div className="tree-children" role="group">
              {kids.map((k) => renderRow(k, depth + 1, null))}
            </div>
          </Collapse>
        )}
      </div>
    )
  }

  const pinnedShown = groups.pinned.filter((d) => isVisible(d.id))
  const libRows = [...groups.folders.map((d) => [d, 'folders'] as const), ...groups.notes.map((d) => [d, 'notes'] as const)]
  const libShown = libRows.filter(([d]) => isVisible(d.id))
  const pinnedOpen = filtered ? true : !closedSections.has('pinned')

  return (
    <div className="lp-tree">
      <div className="lp-tree-filter">
        <Icon name="search" size={14} />
        <input
          ref={filterRef}
          placeholder="Filter titles"
          aria-label="Filter the tree by title"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== 'Escape') return
            // Esc clears the filter, then gives the keyboard back to the page;
            // the global Esc (palettes) must not see either
            e.stopPropagation()
            e.preventDefault()
            if (filter) setFilter('')
            else filterRef.current?.blur()
          }}
          spellCheck={false}
        />
      </div>
      <div
        className="tree-root"
        role="tree"
        aria-label="Docs"
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
        {groups.pinned.length > 0 && (!filtered || pinnedShown.length > 0) && (
          <div className="tree-section">
            <button type="button" className="lp-side-sec tree-section-head" aria-expanded={pinnedOpen} onClick={() => toggleSection('pinned')}>
              {SECTION_LABEL.pinned}
              {!pinnedOpen && <span className="tree-count">{groups.pinned.length}</span>}
            </button>
            <Collapse open={pinnedOpen}>{pinnedShown.map((d) => renderRow(d, 0, 'pinned'))}</Collapse>
          </div>
        )}
        <div className="tree-section">
          <div className="lp-side-sec">
            Library
            <span className="lp-side-sec-r">
              <Link to={{ kind: 'library', workspace }} className="lp-side-sec-link">
                All docs
              </Link>
              {onNewDoc && <IconButton icon="plus" size="sm" label="New doc" onClick={onNewDoc} />}
            </span>
          </div>
          {libShown.map(([d, sec]) => renderRow(d, 0, sec))}
          {!filtered && libRows.length === 0 && <div className="tree-empty">No docs here yet</div>}
        </div>
        {filtered && filtered.matches.size === 0 && <div className="tree-empty">No titles match</div>}
      </div>
    </div>
  )
}
