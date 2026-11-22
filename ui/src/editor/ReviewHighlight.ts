// In-editor review marks: a node decoration on every top-level node whose
// blockId is under review, and (Lapis Doc artboard) the flagged block's own
// verdict bar under it, an inline word diff of what the agent changed, and a
// comment-count bubble at the right edge of a commented block. The maps live
// in plugin state and are swapped without remounting via a transaction meta:
//   setReviewMap(editor, map, extras)
// An edit that touches no block under review or with comments only maps the
// decorations (mapDecos); one that does rebuilds them, and the word diffs
// are cached by (before, now) text, so typing re-diffs at most the block it
// is in.
//
// The bar's buttons carry data-resolve / data-ann; DocView handles the click
// (the editor never talks to the API about review).

import { Extension, type Editor } from '@tiptap/core'
import { Plugin, PluginKey, type EditorState, type Transaction } from '@tiptap/pm/state'
import { Decoration, DecorationSet } from '@tiptap/pm/view'
import type { Node as PMNode } from '@tiptap/pm/model'
import { wordDiff, type DiffSeg } from './docMeta'

/** yellow = applied, flagged; red = proposed, not applied; red-delete = a
 * proposed deletion of this block */
export type ReviewTone = 'yellow' | 'red' | 'red-delete'
export type ReviewMap = Record<string, ReviewTone>

/** What the bar under a block under review says and offers. */
export interface ReviewDetail {
  annotationId: string
  tone: ReviewTone
  who: string
  agent: boolean
  confidence: number | null
  /** the first source ref, e.g. a repo path */
  source: string | null
  /** yellow replace: the block's text before, diffed inline against now */
  beforeText: string | null
  /** red replace: the proposed text, shown in the bar */
  proposedText: string | null
  accept: string
  decline: string
  /** a viewer (read-only) sees the bar without buttons */
  canResolve: boolean
}

export interface ReviewExtras {
  details?: Record<string, ReviewDetail>
  /** blockId → comment count (the right-edge bubble) */
  comments?: Record<string, number>
}

interface PluginState {
  map: ReviewMap
  extras: ReviewExtras
  decos: DecorationSet
}

export const reviewKey = new PluginKey<PluginState>('reviewHighlight')

const SVG = (inner: string, size = 13, sw = 2) =>
  `<svg viewBox="0 0 24 24" width="${size}" height="${size}" fill="none" stroke="currentColor" stroke-width="${sw}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${inner}</svg>`
const ICON = {
  flagged: '<path d="M5 21V4M5 4h11l-2 4 2 4H5"/>',
  parked: '<rect x="4" y="4" width="16" height="16" rx="4"/><path d="M10 9v6M14 9v6"/>',
  comment: '<path d="M5 5h14a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-7l-4 3.5V16H5a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1z"/>',
}
const GLYPH =
  '<svg viewBox="0 0 24 24" width="13" height="13" fill="currentColor" aria-hidden="true"><path d="M12 2.5 14 10l7.5 2-7.5 2-2 7.5-2-7.5L2.5 12 10 10z"/></svg>'

function el(tag: string, cls?: string, text?: string): HTMLElement {
  const e = document.createElement(tag)
  if (cls) e.className = cls
  if (text != null) e.textContent = text
  return e
}

/** The verdict bar under a block under review. */
export function reviewBar(d: ReviewDetail): HTMLElement {
  const yellow = d.tone === 'yellow'
  const bar = el('div', `rv-bar rv-bar-${yellow ? 'yellow' : 'red'}`)
  bar.contentEditable = 'false'
  bar.setAttribute('data-ann-bar', d.annotationId)
  if (!yellow && d.proposedText) {
    const p = el('div', 'rv-proposed')
    p.append(el('span', 'rv-proposed-label', 'Proposed'), el('span', 'rv-proposed-text', d.proposedText))
    bar.append(p)
  }
  const row = el('div', 'rv-row')
  const v = el('span', `lp-verdict lp-verdict-${yellow ? 'yellow' : 'red'}`)
  v.innerHTML = SVG(yellow ? ICON.flagged : ICON.parked)
  v.append(yellow ? 'Flagged · applied' : d.tone === 'red-delete' ? 'Delete parked · not applied' : 'Parked · not applied')
  row.append(v)
  const who = el('span', 'rv-who')
  if (d.agent) {
    const g = el('span', 'lp-avatar lp-avatar-agent')
    g.style.width = g.style.height = '22px'
    g.innerHTML = GLYPH
    who.append(g, el('span', 'lp-agent-name', d.who))
  } else {
    const a = el('span', 'lp-avatar lp-avatar-person', (d.who.trim()[0] ?? '?').toUpperCase())
    a.style.width = a.style.height = '22px'
    a.style.fontSize = '11px'
    who.append(a, el('span', 'rv-person', d.who))
  }
  row.append(who)
  const meta: string[] = []
  if (d.confidence != null) meta.push(`confidence ${d.confidence.toFixed(2)}`)
  if (meta.length || d.source) {
    const m = el('span', 'rv-meta')
    m.append(meta.length ? `· ${meta.join(' · ')}` : '')
    if (d.source) {
      m.append(' · from ')
      m.append(el('span', 'rv-src', d.source))
    }
    row.append(m)
  }
  if (d.canResolve) {
    const acts = el('span', 'rv-actions')
    const keep = el('button', 'lp-btn lp-btn-sm lp-btn-ok', cap(d.accept)) as HTMLButtonElement
    keep.type = 'button'
    keep.dataset.resolve = 'accept'
    keep.dataset.ann = d.annotationId
    const rev = el('button', 'lp-btn lp-btn-sm', cap(d.decline)) as HTMLButtonElement
    rev.type = 'button'
    rev.dataset.resolve = 'decline'
    rev.dataset.ann = d.annotationId
    acts.append(keep, rev)
    row.append(acts)
  }
  bar.append(row)
  return bar
}

function cap(s: string) {
  return s ? s[0].toUpperCase() + s.slice(1) : s
}

function bubble(n: number, blockId: string): HTMLElement {
  const b = el('button', 'cbub') as HTMLButtonElement
  b.contentEditable = 'false'
  b.type = 'button'
  b.dataset.commentsFor = blockId
  b.setAttribute('aria-label', `${n} comment${n === 1 ? '' : 's'} on this block`)
  b.innerHTML = SVG(ICON.comment, 13, 1.75)
  b.append(String(n))
  return b
}

/** Text offset → doc position for a node's text (pos = the node's start). */
function textPositions(node: PMNode, pos: number): number[] {
  const at: number[] = []
  node.descendants((child, p) => {
    if (child.isText && child.text) for (let i = 0; i < child.text.length; i++) at.push(pos + 1 + p + i)
    return true
  })
  return at
}

/** Word diffs by (before, now): a rebuild re-diffs only a block whose text
 * changed. Bounded; the oldest entries go first. */
const diffCache = new Map<string, DiffSeg[]>()
const DIFF_CACHE_MAX = 256
export function cachedWordDiff(before: string, now: string): DiffSeg[] {
  const key = `${before}\u0000${now}`
  let segs = diffCache.get(key)
  if (segs) {
    // most recently used last
    diffCache.delete(key)
    diffCache.set(key, segs)
    return segs
  }
  segs = wordDiff(before, now)
  diffCache.set(key, segs)
  if (diffCache.size > DIFF_CACHE_MAX) diffCache.delete(diffCache.keys().next().value!)
  return segs
}

function diffDecos(node: PMNode, pos: number, before: string, out: Decoration[], id: string) {
  const now = node.textContent
  if (before === now) return
  const at = textPositions(node, pos)
  if (at.length !== now.length) return
  let off = 0
  for (const seg of cachedWordDiff(before, now)) {
    if (seg.t === 'eq') off += seg.s.length
    else if (seg.t === 'ins') {
      const from = at[off]
      const to = at[off + seg.s.length - 1] + 1
      if (seg.s.trim() && from != null && to != null) out.push(Decoration.inline(from, to, { class: 'rv-ins' }))
      off += seg.s.length
    } else if (seg.s.trim()) {
      const where = off < at.length ? at[off] : at.length ? at[at.length - 1] + 1 : pos + 1
      const text = seg.s.trim()
      // drawn only when ProseMirror needs a new one (the key matches the old);
      // keyed within the block, so mapped and rebuilt sets agree
      out.push(Decoration.widget(where, () => el('del', 'rv-del', text), { side: -1, ignoreSelection: true, key: `del-${id}-${where - pos}-${seg.s}` }))
      if (off < at.length) {
        // a space so the struck words don't run into the next ones
        out.push(Decoration.widget(where, () => el('span', 'rv-del-gap', ' '), { side: -1, ignoreSelection: true, key: `gap-${id}-${where - pos}` }))
      }
    }
  }
}

/** Does `tr` change, remove or add a top-level node that carries a
 * decoration (a block under review or with comments)? Checked on both sides
 * of every step, a node beside the change included (an insert at a block's
 * edge can make another node the block's last). An attribute step (the
 * editor's blockId stamp) moves nothing: it gives a new block a fresh id. */
export function touchesDecorated(tr: Transaction, map: ReviewMap, comments: Record<string, number>): boolean {
  const marked = (node: PMNode) => {
    const id = node.attrs?.blockId as string | null | undefined
    return !!id && (!!map[id] || !!comments[id])
  }
  const hits = (doc: PMNode, from: number, to: number) => {
    let hit = false
    const lo = Math.max(0, from - 1)
    const hi = Math.min(doc.content.size, to + 1)
    if (lo > hi) return false
    doc.nodesBetween(lo, hi, (node) => {
      if (marked(node)) hit = true
      return false // top-level nodes only
    })
    return hit
  }
  for (let i = 0; i < tr.steps.length; i++) {
    const before = tr.docs[i]
    const after = i + 1 < tr.docs.length ? tr.docs[i + 1] : tr.doc
    let hit = false
    tr.steps[i].getMap().forEach((oldStart, oldEnd, newStart, newEnd) => {
      if (hit) return
      hit = hits(before, oldStart, oldEnd) || hits(after, newStart, newEnd)
    })
    if (hit) return true
  }
  return false
}

export function buildDecos(state: EditorState, map: ReviewMap, extras: ReviewExtras): DecorationSet {
  const details = extras.details ?? {}
  const comments = extras.comments ?? {}
  if (Object.keys(map).length === 0 && Object.keys(comments).length === 0) return DecorationSet.empty
  const decos: Decoration[] = []
  // a block spanning several nodes gets one bar, after its last node
  const lastPosOf = new Map<string, number>()
  state.doc.forEach((node, pos) => {
    const id = node.attrs?.blockId as string | null | undefined
    if (id) lastPosOf.set(id, pos)
  })
  const bubbled = new Set<string>()
  state.doc.forEach((node, pos) => {
    const id = node.attrs?.blockId as string | null | undefined
    if (!id) return
    const tone = map[id]
    const n = comments[id]
    // inside the block (absolutely placed at its right edge); a code block
    // is a node view with no room for one
    if (n && !bubbled.has(id) && node.type.name !== 'codeBlock' && node.type.name !== 'horizontalRule') {
      bubbled.add(id)
      decos.push(Decoration.widget(pos + 1, () => bubble(n, id), { side: -1, ignoreSelection: true, stopEvent: () => true, key: `cb-${id}-${n}` }))
    }
    if (!tone) return
    const d = details[id]
    const last = lastPosOf.get(id) === pos
    decos.push(Decoration.node(pos, pos + node.nodeSize, { class: `review-${tone}${d && last ? ' review-barred' : ''}` }))
    if (d && tone === 'yellow' && d.beforeText != null) diffDecos(node, pos, d.beforeText, decos, id)
    if (d && last) {
      const key = `bar-${d.annotationId}-${d.canResolve ? 1 : 0}-${d.accept}`
      decos.push(Decoration.widget(pos + node.nodeSize, () => reviewBar(d), { side: -1, ignoreSelection: true, stopEvent: () => true, key }))
    }
  })
  return DecorationSet.create(state.doc, decos)
}

export const ReviewHighlight = Extension.create({
  name: 'reviewHighlight',
  addProseMirrorPlugins() {
    return [
      new Plugin({
        key: reviewKey,
        state: {
          init: () => ({ map: {}, extras: {}, decos: DecorationSet.empty }),
          apply: (tr, old, _oldState, newState) => {
            const next = tr.getMeta(reviewKey) as { map: ReviewMap; extras: ReviewExtras } | undefined
            if (next) return { map: next.map, extras: next.extras, decos: buildDecos(newState, next.map, next.extras) }
            if (tr.docChanged) {
              // typing elsewhere: the decorations only move with the text
              if (!touchesDecorated(tr, old.map, (old.extras as ReviewExtras).comments ?? {}))
                return { ...old, decos: old.decos.map(tr.mapping, tr.doc) }
              return { ...old, decos: buildDecos(newState, old.map, old.extras) }
            }
            return old
          },
        },
        props: {
          decorations(state) {
            return reviewKey.getState(state)?.decos ?? DecorationSet.empty
          },
        },
      }),
    ]
  },
})

/** Push a new map into a mounted editor (no history entry, no dirty flag). */
export function setReviewMap(editor: Editor, map: ReviewMap, extras: ReviewExtras = {}) {
  const tr = editor.state.tr.setMeta(reviewKey, { map, extras }).setMeta('addToHistory', false)
  editor.view.dispatch(tr)
}
