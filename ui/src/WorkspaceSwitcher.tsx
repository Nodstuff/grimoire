// The workspace switcher above the tree (ADR 0004): All / Unsorted / each
// workspace by its display name, and — for a workspace you own — its
// members, with add and remove. Inline UI only (window.confirm is a no-op in
// the app's WKWebView). Sharing is a human surface: this is it, and the box
// CLI (`taisce workspace share`); never MCP.

import { useCallback, useEffect, useState } from 'react'
import { api } from './types'
import { errText, notify } from './Notice'
import { Member, Role, Workspace, WorkspaceSel, canManageMembers, switcherLabel } from './workspaces'

export default function WorkspaceSwitcher({
  value,
  onChange,
}: {
  value: WorkspaceSel
  onChange: (sel: WorkspaceSel) => void
}) {
  const [list, setList] = useState<Workspace[]>([])
  const [members, setMembers] = useState<Member[] | null>(null)
  const [who, setWho] = useState('')
  const [role, setRole] = useState<Role>('viewer')

  const load = useCallback(() => {
    api<{ workspaces: Workspace[] }>('/api/workspaces')
      .then((r) => setList(r.workspaces))
      .catch(() => setList([]))
  }, [])
  useEffect(load, [load])

  const current = list.find((w) => w.id === value)
  // a selection that disappeared (unshared, deleted) falls back to All
  useEffect(() => {
    if (value !== 'all' && value !== 'unsorted' && list.length > 0 && !current) onChange('all')
  }, [value, list, current, onChange])

  const loadMembers = (id: string) =>
    api<{ members: Member[] }>(`/api/workspaces/${id}/members`)
      .then((r) => setMembers(r.members))
      .catch((e) => notify(errText(e)))

  const share = async () => {
    if (!current || !who.trim()) return
    try {
      // the route takes a user id and never says whether one exists; the
      // list below is the truth (names are resolved on the box's CLI)
      await api(`/api/workspaces/${current.id}/members`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ user: who.trim(), role }),
      })
      loadMembers(current.id)
      setWho('')
      load()
    } catch (e) {
      notify(errText(e))
    }
  }

  const unshare = async (m: Member) => {
    if (!current) return
    try {
      await api(`/api/workspaces/${current.id}/members/${m.user_id}`, { method: 'DELETE' })
      loadMembers(current.id)
      load()
    } catch (e) {
      notify(errText(e))
    }
  }

  if (list.length === 0) return null
  return (
    <div className="ws-switcher">
      <select
        className="ws-select"
        value={value}
        onChange={(e) => {
          setMembers(null)
          onChange(e.target.value)
        }}
        title="workspace"
      >
        <option value="all">All workspaces</option>
        <option value="unsorted">Unsorted</option>
        {list.map((w) => (
          <option key={w.id} value={w.id}>
            {switcherLabel(w)}
          </option>
        ))}
      </select>
      {current && canManageMembers(current) && (
        <button
          type="button"
          className="ws-members-btn"
          onClick={() => (members ? setMembers(null) : loadMembers(current.id))}
          title="who can see this workspace"
        >
          {members ? 'done' : 'members'}
        </button>
      )}
      {current && members && (
        <div className="ws-members">
          {members.map((m) => (
            <div key={m.user_id} className="ws-member">
              <span>{m.name}</span>
              <span className="ws-role">{m.role}</span>
              {m.role !== 'owner' && (
                <button type="button" onClick={() => unshare(m)} title="remove from this workspace">
                  remove
                </button>
              )}
            </div>
          ))}
          <div className="ws-member ws-add">
            <input value={who} onChange={(e) => setWho(e.target.value)} placeholder="user id" spellCheck={false} />
            <select value={role} onChange={(e) => setRole(e.target.value as Role)}>
              <option value="viewer">viewer</option>
              <option value="editor">editor</option>
            </select>
            <button type="button" onClick={share}>
              share
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
