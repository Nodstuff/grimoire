import { describe, expect, it } from 'vitest'
import type { Doc } from './types'
import { Workspace, canManageMembers, docsInWorkspace, switcherLabel } from './workspaces'

const ws = (over: Partial<Workspace>): Workspace => ({
  id: 'w1',
  name: 'Work',
  color: null,
  icon: null,
  sort_key: null,
  doc_ids: [],
  ...over,
})

const doc = (id: string, parent: string | null, workspace: string | null): Doc => ({
  id,
  parent_id: parent,
  title: id,
  review_policy: null,
  current_epoch: 1,
  created_by: 'p',
  status: null,
  sort_key: null,
  workspace_id: workspace,
})

describe('switcherLabel', () => {
  it('shows the display name, so a clashing shared workspace carries its owner', () => {
    expect(switcherLabel(ws({ display_name: 'Work · Aoife', shared: true, role: 'editor' }))).toBe('Work · Aoife ·  shared')
  })
  it('falls back to the name for a pre-multi-user daemon', () => {
    expect(switcherLabel(ws({}))).toBe('Work')
  })
  it('marks a viewer read-only', () => {
    expect(switcherLabel(ws({ display_name: 'Family', shared: true, role: 'viewer' }))).toBe('Family ·  shared (read-only)')
  })
})

describe('canManageMembers', () => {
  it('is the owner only', () => {
    expect(canManageMembers(ws({ role: 'owner' }))).toBe(true)
    expect(canManageMembers(ws({ role: 'editor' }))).toBe(false)
    expect(canManageMembers(ws({ role: 'viewer' }))).toBe(false)
    expect(canManageMembers(undefined)).toBe(false)
  })
})

describe('docsInWorkspace', () => {
  const docs = [doc('a', null, null), doc('b', 'a', 'w1'), doc('c', 'b', 'w1'), doc('d', null, 'w2')]
  it('all keeps everything', () => {
    expect(docsInWorkspace(docs, 'all')).toBe(docs)
  })
  it('a workspace keeps its docs and roots a doc whose parent is outside', () => {
    const got = docsInWorkspace(docs, 'w1')
    expect(got.map((d) => d.id)).toEqual(['b', 'c'])
    expect(got[0].parent_id).toBeNull()
    expect(got[1].parent_id).toBe('b')
  })
  it('unsorted keeps unlabelled docs', () => {
    expect(docsInWorkspace(docs, 'unsorted').map((d) => d.id)).toEqual(['a'])
  })
})
