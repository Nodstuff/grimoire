// In-editor review marks: a node decoration on every top-level node whose
// blockId is under review. The map (blockId → tone) lives in plugin state
// and is swapped without remounting via a transaction meta:
//   editor.view.dispatch(editor.state.tr.setMeta(reviewKey, map))
// Decorations are rebuilt on every doc change so they follow the block.

import { Extension, type Editor } from '@tiptap/core'
import { Plugin, PluginKey, type EditorState } from '@tiptap/pm/state'
import { Decoration, DecorationSet } from '@tiptap/pm/view'

/** yellow = applied, flagged; red = proposed, not applied; red-delete = a
 * proposed deletion of this block */
export type ReviewTone = 'yellow' | 'red' | 'red-delete'
export type ReviewMap = Record<string, ReviewTone>

export const reviewKey = new PluginKey<{ map: ReviewMap; decos: DecorationSet }>('reviewHighlight')

function buildDecos(state: EditorState, map: ReviewMap): DecorationSet {
  const decos: Decoration[] = []
  if (Object.keys(map).length === 0) return DecorationSet.empty
  state.doc.forEach((node, pos) => {
    const id = node.attrs?.blockId as string | null | undefined
    const tone = id ? map[id] : undefined
    if (tone) decos.push(Decoration.node(pos, pos + node.nodeSize, { class: `review-${tone}` }))
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
          init: () => ({ map: {}, decos: DecorationSet.empty }),
          apply: (tr, old, _oldState, newState) => {
            const next = tr.getMeta(reviewKey) as ReviewMap | undefined
            if (next) return { map: next, decos: buildDecos(newState, next) }
            if (tr.docChanged) return { map: old.map, decos: buildDecos(newState, old.map) }
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
export function setReviewMap(editor: Editor, map: ReviewMap) {
  const tr = editor.state.tr.setMeta(reviewKey, map).setMeta('addToHistory', false)
  editor.view.dispatch(tr)
}
