// Who can see a workspace you own (ADR 0004): the members, remove, and add
// by user id + role. A human surface only (never MCP): this dialog and the
// box CLI (`taisce workspace share`). Opened from the sidebar's workspace
// menu. Inline confirm, never window.confirm.

import { useCallback, useEffect, useState } from 'react'
import { api } from '../types'
import { errText, notify } from '../Notice'
import type { Member, Role, Workspace } from '../workspaces'
import { Avatar, Button, Chip, ConfirmButton, Dialog, Spinner, TextField } from '../lapis'

export default function WorkspaceMembers({ workspace, onClose, onChanged }: { workspace: Workspace; onClose: () => void; onChanged: () => void }) {
  const [members, setMembers] = useState<Member[] | null>(null)
  const [who, setWho] = useState('')
  const [role, setRole] = useState<Role>('viewer')

  const load = useCallback(
    () =>
      api<{ members: Member[] }>(`/api/workspaces/${workspace.id}/members`)
        .then((r) => setMembers(r.members))
        .catch((e) => notify(errText(e))),
    [workspace.id],
  )
  useEffect(() => {
    load()
  }, [load])

  const share = async () => {
    if (!who.trim()) return
    try {
      // the route takes a user id and never says whether one exists; the
      // list is the truth (names are resolved on the box's CLI)
      await api(`/api/workspaces/${workspace.id}/members`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ user: who.trim(), role }),
      })
      setWho('')
      load()
      onChanged()
    } catch (e) {
      notify(errText(e))
    }
  }

  const unshare = async (m: Member) => {
    try {
      await api(`/api/workspaces/${workspace.id}/members/${m.user_id}`, { method: 'DELETE' })
      load()
      onChanged()
    } catch (e) {
      notify(errText(e))
    }
  }

  return (
    <Dialog title={`Members of ${workspace.display_name || workspace.name}`} description="People who can see this workspace, and what they may do." onClose={onClose}>
      <div className="ws-members">
        {members === null && <Spinner />}
        {members?.map((m) => (
          <div key={m.user_id} className="ws-member">
            <Avatar name={m.name} agent={false} size={24} />
            <span className="ws-member-name">{m.name}</span>
            <Chip tone={m.role === 'owner' ? 'accent' : 'neutral'}>{m.role}</Chip>
            {m.role !== 'owner' && (
              <ConfirmButton confirmLabel="Remove" onConfirm={() => unshare(m)}>
                Remove…
              </ConfirmButton>
            )}
          </div>
        ))}
        <form
          className="ws-add"
          onSubmit={(e) => {
            e.preventDefault()
            share()
          }}
        >
          <TextField label="Add by user id" value={who} onChange={(e) => setWho(e.target.value)} placeholder="user id" spellCheck={false} />
          <label className="ws-role-pick">
            <span className="lp-field-label">Role</span>
            <select value={role} onChange={(e) => setRole(e.target.value as Role)}>
              <option value="viewer">Viewer</option>
              <option value="editor">Editor</option>
            </select>
          </label>
          <Button type="submit" variant="primary" disabled={!who.trim()}>
            Share
          </Button>
        </form>
      </div>
    </Dialog>
  )
}
