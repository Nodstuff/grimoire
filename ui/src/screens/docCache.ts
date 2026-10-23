// Small read caches the Search and Library pages share: a doc's block tree
// (for a hit's heading and a row's excerpt), a doc's last change (author and
// time, from its history), and doc → tags (from /api/graph, the one route
// that carries every doc's tags). Keyed by the doc's epoch, so an edit
// invalidates its entry; nothing here writes.

import { useEffect, useState } from 'react'
import { api, type Block, type BlockNode, type DocTree } from '../types'
import { uuidTime } from './searchText'

const trees = new Map<string, Promise<DocTree | null>>()

/** GET /api/doc/<id>, once per (id, epoch). */
export function loadTree(id: string, epoch = 0): Promise<DocTree | null> {
  const key = `${id}@${epoch}`
  let p = trees.get(key)
  if (!p) {
    p = api<DocTree>(`/api/doc/${id}`).catch(() => null)
    trees.set(key, p)
    if (trees.size > 300) trees.delete(trees.keys().next().value as string)
  }
  return p
}

/** The heading a block sits under (nearest heading ancestor, or the block
 * itself when it is one), as text without the hashes. */
export function headingOf(roots: BlockNode[], blockId: string): string | null {
  const walk = (nodes: BlockNode[], trail: Block[]): Block[] | null => {
    for (const n of nodes) {
      if (n.block.id === blockId) return [...trail, n.block]
      const r = walk(n.children, [...trail, n.block])
      if (r) return r
    }
    return null
  }
  const path = walk(roots, [])
  if (!path) return null
  for (let i = path.length - 1; i >= 0; i--) {
    const b = path[i]
    if (b.block_type === 'heading' || /^#{1,6}\s/.test(b.content)) {
      const t = b.content.split('\n')[0].replace(/^#{1,6}\s+/, '').trim()
      if (t) return t
    }
  }
  return null
}

/** The first line of prose in a doc (the Library row's second line, a
 * folder's lead): skips frontmatter, headings, rules and empty blocks. */
export function excerptOf(roots: BlockNode[], max = 90): string | null {
  const stack = [...roots]
  while (stack.length) {
    const n = stack.shift()!
    const c = n.block.content.trim()
    const skip = !c || n.block.deleted || /^#{1,6}\s/.test(c) || c.startsWith('---') || n.block.block_type === 'comment'
    if (!skip) {
      const line = c
        .split('\n')[0]
        .replace(/^[-*+>]\s+(\[[ xX]\]\s+)?/, '')
        .replace(/\*\*|__|`/g, '')
        .replace(/\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|([^\]]+))?\]\]/g, (_, t, a) => a || t)
        .trim()
      if (line) return line.length > max ? `${line.slice(0, max - 1).trimEnd()}…` : line
    }
    stack.unshift(...n.children)
  }
  return null
}

/* ---------- last change ---------- */

export interface LastChange {
  who: string
  agent: boolean
  /** ms since epoch, from the op's UUIDv7 */
  at: number | null
}

interface HistoryRow {
  op: { id: string }
  principal_name?: string
  principal_kind?: string
  principal_is_yours?: boolean
}

const lastChanges = new Map<string, Promise<LastChange | null>>()

/** Who made a doc's latest change, and when (GET /api/doc/<id>/history,
 * newest first). An empty history (a folder never written in) → null. */
export function loadLastChange(id: string, epoch = 0): Promise<LastChange | null> {
  const key = `${id}@${epoch}`
  let p = lastChanges.get(key)
  if (!p) {
    p = api<HistoryRow[]>(`/api/doc/${id}/history`)
      .then((rows) => {
        const r = Array.isArray(rows) ? rows[0] : undefined
        if (!r) return null
        const agent = r.principal_kind === 'agent'
        return { who: agent ? (r.principal_name ?? 'agent') : r.principal_is_yours === false ? (r.principal_name ?? 'someone') : 'You', agent, at: uuidTime(r.op.id) }
      })
      .catch(() => null)
    lastChanges.set(key, p)
    if (lastChanges.size > 300) lastChanges.delete(lastChanges.keys().next().value as string)
  }
  return p
}

/* ---------- tags ---------- */

let tagMap: { at: number; p: Promise<Map<string, string[]>> } | null = null

/** doc id → its tags. Refetched when `version` (App's dataVersion) moves. */
export function loadDocTags(version = 0): Promise<Map<string, string[]>> {
  if (!tagMap || tagMap.at !== version) {
    tagMap = {
      at: version,
      p: api<{ nodes: { id: string; tags: string[] }[] }>('/api/graph')
        .then((g) => new Map((g?.nodes ?? []).map((n) => [n.id, n.tags ?? []])))
        .catch(() => new Map()),
    }
  }
  return tagMap.p
}

export function useDocTags(version: number): Map<string, string[]> {
  const [m, setM] = useState<Map<string, string[]>>(new Map())
  useEffect(() => {
    let live = true
    loadDocTags(version).then((x) => live && setM(x))
    return () => {
      live = false
    }
  }, [version])
  return m
}
