// The workspace switcher at the top of the sidebar (canvas Main/Library):
// a card-like button with the workspace's dot, name and a sub-line, opening
// a menu of All / Unsorted / each workspace, then "Members…" for one you
// own. The choice filters the sidebar tree and the Library (App owns it).

import { useEffect, useRef, useState } from 'react'
import { Chip, Icon } from '../lapis'
import { errText, notify } from '../Notice'
import { api } from '../types'
import { canManageMembers, workspaceColor, workspaceSummary, type Workspace, type WorkspaceSel } from '../workspaces'

export default function WorkspaceMenu({
  value,
  list,
  docCount,
  onChange,
  onMembers,
  onCreated,
}: {
  value: WorkspaceSel
  list: Workspace[]
  docCount: number
  onChange: (sel: WorkspaceSel) => void
  onMembers: (w: Workspace) => void
  /** a workspace was made here (POST /api/workspaces): reload the list */
  onCreated?: (id: string) => void
}) {
  const [open, setOpen] = useState(false)
  // "New workspace…": the row turns into a name field in place
  const [naming, setNaming] = useState(false)
  const [newName, setNewName] = useState('')
  const [making, setMaking] = useState(false)
  useEffect(() => {
    if (!open) {
      setNaming(false)
      setNewName('')
    }
  }, [open])
  const makeWorkspace = async () => {
    const name = newName.trim()
    if (!name || making) return
    setMaking(true)
    try {
      const w = await api<{ id: string }>('/api/workspaces', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      })
      // the list reloads; the new workspace is then one click away here
      onCreated?.(w.id)
      setNaming(false)
      setNewName('')
      notify(`made the workspace “${name}”`, 'ok')
    } catch (e) {
      notify(errText(e))
    } finally {
      setMaking(false)
    }
  }
  const box = useRef<HTMLDivElement>(null)
  const btn = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (!open) return
    const off = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false)
    }
    window.addEventListener('mousedown', off)
    box.current?.querySelector<HTMLElement>('[role="menuitemradio"][aria-checked="true"]')?.focus()
    return () => window.removeEventListener('mousedown', off)
  }, [open])

  const idx = list.findIndex((w) => w.id === value)
  const current = idx >= 0 ? list[idx] : undefined
  const { name, sub } = workspaceSummary(value, list, docCount)
  const allDot =
    list.length > 0
      ? `conic-gradient(${list
          .slice(0, 3)
          .map((w, i, a) => `${workspaceColor(w, i)} 0 ${Math.round(((i + 1) / a.length) * 100)}%`)
          .join(', ')})`
      : 'var(--line2)'
  const dot = value === 'all' ? allDot : current ? workspaceColor(current, idx) : 'var(--line2)'

  const pick = (sel: WorkspaceSel) => {
    onChange(sel)
    setOpen(false)
    btn.current?.focus()
  }

  const onMenuKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.stopPropagation()
      setOpen(false)
      btn.current?.focus()
      return
    }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
    e.preventDefault()
    const items = Array.from(box.current?.querySelectorAll<HTMLElement>('.lp-wsmenu [role^="menuitem"]') ?? [])
    const i = items.indexOf(document.activeElement as HTMLElement)
    const n = e.key === 'ArrowDown' ? Math.min(i + 1, items.length - 1) : Math.max(i - 1, 0)
    items[n]?.focus()
  }

  const row = (sel: WorkspaceSel, label: string, dotColor: string, extra?: React.ReactNode) => (
    <button
      key={sel}
      type="button"
      role="menuitemradio"
      aria-checked={value === sel}
      className={value === sel ? 'sel' : ''}
      onClick={() => pick(sel)}
    >
      <span className="lp-dot" style={{ background: dotColor }} aria-hidden />
      <span className="lp-wsmenu-name">{label}</span>
      {extra}
      {value === sel && <Icon name="check" size={15} className="lp-wsmenu-check" />}
    </button>
  )

  return (
    <div className="lp-ws" ref={box}>
      <button
        ref={btn}
        type="button"
        className="lp-ws-btn"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={`Workspace: ${name}`}
        onClick={() => setOpen((o) => !o)}
      >
        <span className="lp-dot" style={{ background: dot }} aria-hidden />
        <span className="lp-ws-text">
          <span className="lp-ws-name">{name}</span>
          {sub && <span className="lp-ws-sub">{sub}</span>}
        </span>
        <Icon name={open ? 'chevronUp' : 'chevronDown'} size={15} />
      </button>
      {open && (
        <div className="lp-wsmenu" role="menu" aria-label="Switch workspace" onKeyDown={onMenuKey}>
          {row('all', 'All workspaces', allDot)}
          {row('unsorted', 'Unsorted', 'var(--line2)')}
          {list.map((w, i) =>
            row(
              w.id,
              w.display_name || w.name,
              workspaceColor(w, i),
              w.shared || (w.role && w.role !== 'owner') ? (
                <Chip className="lp-wsmenu-pill">{`shared${w.role && w.role !== 'owner' ? ` · ${w.role}` : ''}`}</Chip>
              ) : undefined,
            ),
          )}
          <div className="lp-wsmenu-sep" role="separator" />
          {onCreated &&
            (naming ? (
              <form
                className="lp-wsmenu-new"
                onSubmit={(e) => {
                  e.preventDefault()
                  makeWorkspace()
                }}
              >
                <Icon name="plus" size={15} />
                <input
                  autoFocus
                  aria-label="New workspace name"
                  placeholder="Name the workspace"
                  value={newName}
                  disabled={making}
                  onChange={(e) => setNewName(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Escape') {
                      e.stopPropagation()
                      setNaming(false)
                    }
                  }}
                />
              </form>
            ) : (
              <button type="button" role="menuitem" onClick={() => setNaming(true)}>
                <Icon name="plus" size={15} />
                <span className="lp-wsmenu-name">New workspace…</span>
              </button>
            ))}
          {current && canManageMembers(current) && (
            <>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setOpen(false)
                  onMembers(current)
                }}
              >
                <Icon name="members" size={15} />
                <span className="lp-wsmenu-name">Manage members of {current.display_name || current.name}…</span>
              </button>
            </>
          )}
        </div>
      )}
    </div>
  )
}
