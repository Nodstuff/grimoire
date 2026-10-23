// Settings › Workspaces & members (canvas Settings, #workspaces): one row
// per workspace (colour, name, your role · docs · who else sees it) with
// Rename and Colour; on a workspace you own in SERVER mode, Share… opens its
// members under the row: role (Editor / Viewer), Remove, and Add. New
// workspace at the foot. Only people share (ADR 0004): this is a human
// surface, never MCP.
//
// Adding a member takes a user id: POST /api/workspaces/{id}/members
// resolves ids only and never says whether an account exists (names are
// resolved on the box, `taisce auth user list`), so a name typed here could
// not be checked. Changing a role re-posts the member (the store upserts).

import { useCallback, useEffect, useId, useState } from 'react'
import { api } from '../types'
import { errText, notify } from '../Notice'
import { canManageMembers, workspaceColor, type Member, type Role, type Workspace } from '../workspaces'
import { Avatar, Button, ConfirmButton, Icon, Spinner } from '../lapis'
import { Row, SettingsCard } from './settingsParts'

/** The colours offered for a workspace (the token palette, light values:
 * a stored colour is a hex). */
export const WORKSPACE_SWATCHES = ['#3346D3', '#17804A', '#D9692A', '#7535CC', '#0F7C8C', '#C2336B']

const JSON_HEADERS = { 'Content-Type': 'application/json' }

/** "Owner · 38 docs · only you", "Owner · 22 docs · shared with 2 people", "Editor · Aoife's". */
export function workspaceLine(w: Workspace, members: Member[] | undefined, server: boolean): string {
  const role = w.role ? w.role[0].toUpperCase() + w.role.slice(1) : 'Owner'
  const n = w.doc_count ?? w.doc_ids.length
  const parts = [role, `${n} doc${n === 1 ? '' : 's'}`]
  if (w.role && w.role !== 'owner') {
    if (w.owner_name) parts.push(`shared by ${w.owner_name}`)
  } else if (server) {
    const others = members ? members.filter((m) => m.role !== 'owner').length : w.shared ? null : 0
    parts.push(others === null ? 'shared' : others === 0 ? 'only you' : `shared with ${others} ${others === 1 ? 'person' : 'people'}`)
  }
  return parts.join(' · ')
}

export default function SettingsWorkspaces({ workspaces, server, onChanged }: { workspaces: Workspace[]; server: boolean; onChanged: () => void }) {
  const [members, setMembers] = useState<Record<string, Member[]>>({})
  const [sharing, setSharing] = useState<string | null>(null)
  const [renaming, setRenaming] = useState<string | null>(null)
  const [colouring, setColouring] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)

  const loadMembers = useCallback((id: string) => {
    api<{ members: Member[] }>(`/api/workspaces/${id}/members`)
      .then((r) => setMembers((m) => ({ ...m, [id]: r.members })))
      .catch(() => {})
  }, [])
  // who else sees each workspace you own (SERVER mode; LOCAL is single-user)
  const owned = workspaces.filter((w) => canManageMembers(w))
  const ownedKey = owned.map((w) => w.id).join(',')
  useEffect(() => {
    if (!server) return
    owned.forEach((w) => loadMembers(w.id))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [server, ownedKey, loadMembers])

  const patch = async (w: Workspace, body: Record<string, unknown>) => {
    try {
      await api(`/api/workspaces/${w.id}`, { method: 'PATCH', headers: JSON_HEADERS, body: JSON.stringify(body) })
      onChanged()
      return true
    } catch (e) {
      notify(errText(e))
      return false
    }
  }

  return (
    <SettingsCard id="workspaces" title="Workspaces & members" sub="Only people can share. Agents act for you and never change who can see what; their edits in a shared workspace always land flagged for the members.">
      <div>
        {workspaces.length === 0 && (
          <Row first>
            <span className="muted">No workspaces yet: everything is in Unsorted.</span>
          </Row>
        )}
        {workspaces.map((w, i) => {
          const mine = canManageMembers(w)
          const open = sharing === w.id
          return (
            <Row key={w.id} first={i === 0} align={open || renaming === w.id || colouring === w.id ? 'start' : undefined}>
              <span className="st-ws-dot" style={{ background: workspaceColor(w, i) }} aria-hidden />
              <div className="st-ws-main">
                {renaming === w.id ? (
                  <RenameForm
                    initial={w.name}
                    onCancel={() => setRenaming(null)}
                    onSave={async (name) => {
                      if (await patch(w, { name })) setRenaming(null)
                    }}
                  />
                ) : (
                  <span className="st-row-text">
                    <b className="st-strong st-ws-name">{w.display_name || w.name}</b>
                    <span className="st-row-sub">{workspaceLine(w, members[w.id], server)}</span>
                  </span>
                )}
                {colouring === w.id && (
                  <div className="st-swatches" role="radiogroup" aria-label={`Colour for ${w.name}`}>
                    {WORKSPACE_SWATCHES.map((c) => (
                      <button
                        key={c}
                        type="button"
                        role="radio"
                        aria-checked={(w.color ?? '').toLowerCase() === c.toLowerCase()}
                        aria-label={c}
                        className="st-swatch"
                        style={{ background: c }}
                        onClick={async () => {
                          if (await patch(w, { color: c })) setColouring(null)
                        }}
                      />
                    ))}
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={async () => {
                        if (await patch(w, { color: null })) setColouring(null)
                      }}
                    >
                      Default
                    </Button>
                  </div>
                )}
                {open && <Members workspace={w} list={members[w.id]} onChanged={() => loadMembers(w.id)} />}
              </div>
              {mine && renaming !== w.id && (
                <span className="st-ws-actions">
                  <Button variant="ghost" size="sm" onClick={() => setRenaming(w.id)}>
                    Rename
                  </Button>
                  <Button variant="ghost" size="sm" aria-expanded={colouring === w.id} onClick={() => setColouring(colouring === w.id ? null : w.id)}>
                    Colour
                  </Button>
                  {server && (
                    <Button size="sm" aria-expanded={open} onClick={() => setSharing(open ? null : w.id)}>
                      {open ? 'Done' : 'Share…'}
                    </Button>
                  )}
                </span>
              )}
            </Row>
          )
        })}
        <Row>
          {creating ? (
            <RenameForm
              initial=""
              label="New workspace name"
              saveLabel="Create"
              onCancel={() => setCreating(false)}
              onSave={async (name) => {
                try {
                  await api('/api/workspaces', { method: 'POST', headers: JSON_HEADERS, body: JSON.stringify({ name }) })
                  setCreating(false)
                  onChanged()
                } catch (e) {
                  notify(errText(e))
                }
              }}
            />
          ) : (
            <Button size="sm" icon="plus" onClick={() => setCreating(true)}>
              New workspace
            </Button>
          )}
        </Row>
      </div>
    </SettingsCard>
  )
}

function RenameForm({ initial, onSave, onCancel, label = 'Workspace name', saveLabel = 'Save' }: { initial: string; onSave: (name: string) => void; onCancel: () => void; label?: string; saveLabel?: string }) {
  const [name, setName] = useState(initial)
  const fid = useId()
  const ok = name.trim() && name.trim() !== initial
  return (
    <form
      className="st-inline-form st-grow"
      onSubmit={(e) => {
        e.preventDefault()
        if (ok) onSave(name.trim())
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') {
          e.stopPropagation()
          onCancel()
        }
      }}
    >
      <label className="sr-only" htmlFor={fid}>
        {label}
      </label>
      <input id={fid} className="st-inp st-inp-sm st-grow" autoFocus value={name} placeholder={label} onChange={(e) => setName(e.target.value)} />
      <Button type="submit" size="sm" variant="primary" disabled={!ok}>
        {saveLabel}
      </Button>
      <Button size="sm" variant="ghost" onClick={onCancel}>
        Cancel
      </Button>
    </form>
  )
}

function Members({ workspace, list, onChanged }: { workspace: Workspace; list: Member[] | undefined; onChanged: () => void }) {
  const [who, setWho] = useState('')
  const [role, setRole] = useState<Role>('viewer')

  const post = async (user: string, r: Role) => {
    try {
      await api(`/api/workspaces/${workspace.id}/members`, { method: 'POST', headers: JSON_HEADERS, body: JSON.stringify({ user, role: r }) })
      onChanged()
      return true
    } catch (e) {
      notify(errText(e))
      return false
    }
  }
  const remove = async (m: Member) => {
    try {
      await api(`/api/workspaces/${workspace.id}/members/${m.user_id}`, { method: 'DELETE' })
      onChanged()
    } catch (e) {
      notify(errText(e))
    }
  }

  const others = list?.filter((m) => m.role !== 'owner') ?? []
  const addId = `st-add-${workspace.id}`
  return (
    <div className="st-members">
      {list === undefined && <Spinner />}
      {list !== undefined && others.length === 0 && <span className="muted st-members-none">Only you can see it.</span>}
      {others.map((m) => (
        <div key={m.user_id} className="st-member">
          <Avatar name={m.name} agent={false} size={26} />
          <span className="st-member-name">{m.name}</span>
          <label className="sr-only" htmlFor={`st-role-${workspace.id}-${m.user_id}`}>
            {m.name}’s role
          </label>
          <select id={`st-role-${workspace.id}-${m.user_id}`} className="st-role" value={m.role} onChange={(e) => post(m.user_id, e.target.value as Role)}>
            <option value="editor">Editor</option>
            <option value="viewer">Viewer</option>
          </select>
          <ConfirmButton variant="ghost" confirmLabel="Remove" onConfirm={() => remove(m)}>
            Remove
          </ConfirmButton>
        </div>
      ))}
      <form
        className="st-member-add"
        onSubmit={async (e) => {
          e.preventDefault()
          // the route never says whether the id exists; the refreshed list is the truth
          if (who.trim() && (await post(who.trim(), role))) setWho('')
        }}
      >
        <label htmlFor={addId} className="sr-only">
          Add a person by user id
        </label>
        <input id={addId} className="st-inp st-inp-sm" placeholder="Add a person by user id" value={who} onChange={(e) => setWho(e.target.value)} spellCheck={false} />
        <select className="st-role" aria-label="Role for the new member" value={role} onChange={(e) => setRole(e.target.value as Role)}>
          <option value="viewer">Viewer</option>
          <option value="editor">Editor</option>
        </select>
        <Button type="submit" size="sm" disabled={!who.trim()}>
          Add
        </Button>
      </form>
      <span className="st-members-help">
        <Icon name="info" size={13} /> People are added by user id (on the server: <span className="mono">taisce auth user list</span>). Someone unknown simply does not appear.
      </span>
    </div>
  )
}
