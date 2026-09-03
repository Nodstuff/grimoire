// Agents in the room (room.rs): an agent's writing arrives as top-level nodes
// carrying `suggestion` attrs. Nothing it wrote becomes doc text without a
// click — the daemon's flatten skips unaccepted suggestions. This extension
// declares the attrs on every top-level node type (so Yjs ↔ ProseMirror
// round-trips them) and paints each suggestion with an inline ✓ / ✗ bar.
//
// Attrs (set by the daemon, cleared here):
//   suggestion:   'insert' | 'replace' — new text, pending
//                 'replaced'           — the still-live original of a replace
//   suggestionId: groups the new node(s) with their replaced original
//   suggestionBy: agent name for the label

import { Extension } from '@tiptap/core'
import { Plugin, PluginKey, type EditorState, type Transaction } from '@tiptap/pm/state'
import { Decoration, DecorationSet, type EditorView } from '@tiptap/pm/view'
import type { Node as PMNode } from '@tiptap/pm/model'

export const SUGGESTION_TYPES = [
  'paragraph',
  'heading',
  'codeBlock',
  'blockquote',
  'bulletList',
  'orderedList',
  'horizontalRule',
  'table',
]

export type SuggestionKind = 'insert' | 'replace' | 'replaced'

export interface SuggestionNode {
  pos: number
  node: PMNode
  kind: SuggestionKind
  id: string
  by: string
}

/** Every top-level node that is part of a suggestion, in document order. */
export function collectSuggestions(doc: PMNode): SuggestionNode[] {
  const out: SuggestionNode[] = []
  doc.forEach((node, pos) => {
    const kind = node.attrs.suggestion as SuggestionKind | null
    if (kind === 'insert' || kind === 'replace' || kind === 'replaced') {
      out.push({ pos, node, kind, id: String(node.attrs.suggestionId ?? ''), by: String(node.attrs.suggestionBy ?? 'agent') })
    }
  })
  return out
}

const CLEAR = { suggestion: null, suggestionId: null, suggestionBy: null }

/** Accept one suggestion group: the new node(s) become plain text; a replaced
 * original is deleted. Returns the transaction to dispatch (or null). */
export function acceptSuggestion(state: EditorState, id: string): Transaction | null {
  const group = collectSuggestions(state.doc).filter((s) => s.id === id)
  if (group.length === 0) return null
  const tr = state.tr
  // bottom-up so positions stay valid
  for (const s of [...group].reverse()) {
    if (s.kind === 'replaced') tr.delete(s.pos, s.pos + s.node.nodeSize)
    else tr.setNodeMarkup(s.pos, undefined, { ...s.node.attrs, ...CLEAR })
  }
  return tr
}

/** Reject one suggestion group: the new node(s) vanish; a replaced original
 * is unmarked and stays. */
export function rejectSuggestion(state: EditorState, id: string): Transaction | null {
  const group = collectSuggestions(state.doc).filter((s) => s.id === id)
  if (group.length === 0) return null
  const tr = state.tr
  for (const s of [...group].reverse()) {
    if (s.kind === 'replaced') tr.setNodeMarkup(s.pos, undefined, { ...s.node.attrs, ...CLEAR })
    else tr.delete(s.pos, s.pos + s.node.nodeSize)
  }
  return tr
}

/** Ids of pending suggestions (one per group), in document order. */
export function pendingSuggestionIds(doc: PMNode): string[] {
  const seen = new Set<string>()
  for (const s of collectSuggestions(doc)) if (s.kind !== 'replaced') seen.add(s.id)
  return [...seen]
}

export const suggestionsKey = new PluginKey<DecorationSet>('suggestions')

function bar(view: EditorView, s: SuggestionNode): HTMLElement {
  const el = document.createElement('div')
  el.className = 'suggestion-bar'
  el.contentEditable = 'false'
  const label = document.createElement('span')
  label.className = 'suggestion-label'
  label.textContent = `🌿 ${s.by} suggests${s.kind === 'replace' ? ' a replacement' : ''}`
  el.appendChild(label)
  if (view.editable) {
    const ok = document.createElement('button')
    ok.className = 'suggestion-accept'
    ok.textContent = '✓ accept'
    ok.onmousedown = (e) => {
      e.preventDefault()
      const tr = acceptSuggestion(view.state, s.id)
      if (tr) view.dispatch(tr)
    }
    const no = document.createElement('button')
    no.className = 'suggestion-reject'
    no.textContent = '✗ reject'
    no.onmousedown = (e) => {
      e.preventDefault()
      const tr = rejectSuggestion(view.state, s.id)
      if (tr) view.dispatch(tr)
    }
    el.appendChild(ok)
    el.appendChild(no)
  }
  return el
}

function buildDecos(state: EditorState): DecorationSet {
  const all = collectSuggestions(state.doc)
  if (all.length === 0) return DecorationSet.empty
  const decos: Decoration[] = []
  // one bar per group, under the LAST new node of that group
  const lastNewOf = new Map<string, SuggestionNode>()
  for (const s of all) {
    decos.push(Decoration.node(s.pos, s.pos + s.node.nodeSize, { class: `suggestion suggestion-${s.kind}` }))
    if (s.kind !== 'replaced') lastNewOf.set(s.id, s)
  }
  for (const s of lastNewOf.values()) {
    decos.push(Decoration.widget(s.pos + s.node.nodeSize, (view) => bar(view, s), { side: 1, key: `sugg-${s.id}` }))
  }
  return DecorationSet.create(state.doc, decos)
}

export const Suggestions = Extension.create({
  name: 'suggestions',
  addGlobalAttributes() {
    const attr = (name: string, dataName: string) => ({
      default: null,
      keepOnSplit: false,
      parseHTML: (el: HTMLElement) => el.getAttribute(dataName),
      renderHTML: (attrs: Record<string, unknown>) => (attrs[name] ? { [dataName]: String(attrs[name]) } : {}),
    })
    return [
      {
        types: SUGGESTION_TYPES,
        attributes: {
          suggestion: attr('suggestion', 'data-suggestion'),
          suggestionId: attr('suggestionId', 'data-suggestion-id'),
          suggestionBy: attr('suggestionBy', 'data-suggestion-by'),
        },
      },
    ]
  },
  addProseMirrorPlugins() {
    return [
      new Plugin({
        key: suggestionsKey,
        state: {
          init: (_, state) => buildDecos(state),
          apply: (tr, old, _oldState, newState) => (tr.docChanged ? buildDecos(newState) : old),
        },
        props: {
          decorations(state) {
            return suggestionsKey.getState(state) ?? DecorationSet.empty
          },
        },
      }),
    ]
  },
})
