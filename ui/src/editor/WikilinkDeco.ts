// Wikilink decorations: style [[Target]] / [[Path/Target|alias]] inside the
// live editor without touching the text. Brackets + target path dim; the
// visible name gets link styling and a data-target for click navigation.

import { Extension } from '@tiptap/core'
import { Plugin, PluginKey } from '@tiptap/pm/state'
import { Decoration, DecorationSet } from '@tiptap/pm/view'
import type { Node as PMNode } from '@tiptap/pm/model'

const RE = /\[\[([^\]|]+)(\|([^\]]+))?\]\]/g

function buildDecos(doc: PMNode): DecorationSet {
  const decos: Decoration[] = []
  doc.descendants((node, pos) => {
    if (!node.isText || !node.text) return
    RE.lastIndex = 0
    let m: RegExpExecArray | null
    while ((m = RE.exec(node.text))) {
      const start = pos + m.index
      const target = m[1].trim()
      const alias = m[3]?.trim()
      const openEnd = start + 2 // after [[
      const targetEnd = openEnd + m[1].length
      if (alias) {
        // [[path|alias]] → dim "[[path|", link-style the alias, dim "]]"
        decos.push(Decoration.inline(start, targetEnd + 1, { class: 'wl-dim' }))
        decos.push(
          Decoration.inline(targetEnd + 1, targetEnd + 1 + m[3]!.length, {
            class: 'wl-target',
            'data-target': target,
          }),
        )
        decos.push(
          Decoration.inline(targetEnd + 1 + m[3]!.length, start + m[0].length, { class: 'wl-dim' }),
        )
      } else {
        decos.push(Decoration.inline(start, openEnd, { class: 'wl-dim' }))
        decos.push(
          Decoration.inline(openEnd, targetEnd, { class: 'wl-target', 'data-target': target }),
        )
        decos.push(Decoration.inline(targetEnd, start + m[0].length, { class: 'wl-dim' }))
      }
    }
  })
  return DecorationSet.create(doc, decos)
}

export const WikilinkDeco = Extension.create({
  name: 'wikilinkDeco',
  addProseMirrorPlugins() {
    return [
      new Plugin({
        key: new PluginKey('wikilinkDeco'),
        state: {
          init: (_, state) => buildDecos(state.doc),
          apply: (tr, old) => (tr.docChanged ? buildDecos(tr.doc) : old),
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
