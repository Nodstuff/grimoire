// Wikilink decorations: style [[Target]] / [[Path/Target|alias]] inside the
// live editor without touching the text. Obsidian live-preview behavior:
// the raw syntax is HIDDEN and only the display name shows as a link —
// unless the caret sits inside the link, when the full syntax reappears
// for editing.

import { Extension } from '@tiptap/core'
import { Plugin, PluginKey, type EditorState } from '@tiptap/pm/state'
import { Decoration, DecorationSet } from '@tiptap/pm/view'

const RE = /\[\[([^\]|]+)(\|([^\]]+))?\]\]/g

function buildDecos(state: EditorState): DecorationSet {
  const decos: Decoration[] = []
  const selFrom = state.selection.from
  const selTo = state.selection.to
  state.doc.descendants((node, pos) => {
    if (!node.isText || !node.text) return
    RE.lastIndex = 0
    let m: RegExpExecArray | null
    while ((m = RE.exec(node.text))) {
      const start = pos + m.index
      const end = start + m[0].length
      const target = m[1].trim()
      const alias = m[3]?.trim()
      const openEnd = start + 2
      const targetEnd = openEnd + m[1].length
      // caret inside the link → show the raw syntax (dimmed) for editing
      const active = selFrom <= end && selTo >= start
      const syntaxClass = active ? 'wl-dim' : 'wl-hide'
      const linkAttrs = { class: 'wl-target', 'data-target': target }
      if (alias) {
        decos.push(Decoration.inline(start, targetEnd + 1, { class: syntaxClass }))
        decos.push(Decoration.inline(targetEnd + 1, targetEnd + 1 + m[3]!.length, linkAttrs))
        decos.push(Decoration.inline(targetEnd + 1 + m[3]!.length, end, { class: syntaxClass }))
      } else {
        decos.push(Decoration.inline(start, openEnd, { class: syntaxClass }))
        decos.push(Decoration.inline(openEnd, targetEnd, linkAttrs))
        decos.push(Decoration.inline(targetEnd, end, { class: syntaxClass }))
      }
    }
  })
  return DecorationSet.create(state.doc, decos)
}

export const WikilinkDeco = Extension.create({
  name: 'wikilinkDeco',
  addProseMirrorPlugins() {
    return [
      new Plugin({
        key: new PluginKey('wikilinkDeco'),
        state: {
          init: (_, state) => buildDecos(state),
          apply: (tr, old, _oldState, newState) =>
            tr.docChanged || tr.selectionSet ? buildDecos(newState) : old,
        },
        props: {
          decorations(state) {
            return this.getState(state)
          },
        },
      }),
    ]
  },
})
