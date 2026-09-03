// The 5.1 editor: one always-live Tiptap surface per doc. No modes, no
// per-block editing — the doc is the editor. Autosave diffs top-level nodes
// against the loaded baseline and proposes block ops through the gate.

import { useEffect, useMemo, useRef, useState } from 'react'
import { EditorContent, ReactNodeViewRenderer, useEditor } from '@tiptap/react'
import { Extension, getSchema } from '@tiptap/core'
import StarterKit from '@tiptap/starter-kit'
import CodeBlock from '@tiptap/extension-code-block'
import { TableKit } from '@tiptap/extension-table'
import CodeBlockView from './CodeBlockView'
import { WikilinkDeco } from './WikilinkDeco'
import { ReviewHighlight, ReviewMap, setReviewMap } from './ReviewHighlight'
import { Suggestions } from './Suggestions'
import type { Node as PMNode } from '@tiptap/pm/model'
import { api, Block } from '../types'
import { notify } from '../Notice'
import { proposeErrorText, saveErrorText } from '../hints'
import { BaselineBlock, Entry, computeOps } from './diff'
import { makeParser, makeSerializer, nodesToMarkdown } from './markdown'

const TOP_LEVEL_TYPES = [
  'paragraph',
  'heading',
  'codeBlock',
  'blockquote',
  'bulletList',
  'orderedList',
  'horizontalRule',
  'table',
]

/** blockId rides on every top-level node; splits/new nodes get null. */
const BlockId = Extension.create({
  name: 'blockId',
  addGlobalAttributes() {
    return [
      {
        types: TOP_LEVEL_TYPES,
        attributes: {
          blockId: {
            default: null,
            keepOnSplit: false,
            // rendered into the DOM so [[Doc#^uuid]] anchors can find blocks
            parseHTML: (el: HTMLElement) => el.getAttribute('data-block-id'),
            renderHTML: (attrs: Record<string, unknown>) =>
              attrs.blockId ? { 'data-block-id': String(attrs.blockId) } : {},
          },
        },
      },
    ]
  },
})

/** The editor extension set. `history: false` builds the variant for the live
 * (Yjs) editor: StarterKit bundles undo/redo as a nested extension, so it must
 * be switched off at configure time — filtering the array by name never
 * matches it (the top-level name is 'starterKit'). Yjs owns history in
 * collab mode; two history plugins would fight. Schema is identical either
 * way (undo/redo adds no nodes or marks). */
export function makeExtensions({ history }: { history: boolean }) {
  return [
    StarterKit.configure({
      link: { openOnClick: false },
      codeBlock: false,
      ...(history ? {} : { undoRedo: false }),
    }),
    CodeBlock.extend({
      addNodeView() {
        return ReactNodeViewRenderer(CodeBlockView)
      },
    }),
    TableKit.configure({ table: { resizable: false } }),
    WikilinkDeco,
    BlockId,
    ReviewHighlight,
    Suggestions,
  ]
}
export const extensions = makeExtensions({ history: true })
export const schema = getSchema(extensions)
export const parser = makeParser(schema)
export const serializer = makeSerializer()

export interface EditableDoc {
  docId: string
  epoch: number
  /** pre-order flattened live blocks, comments and frontmatter excluded */
  blocks: Block[]
}

type SaveState = 'clean' | 'dirty' | 'saving'

/** direct = normal gate save; readonly = view-only mirror; propose = mirror
 * with propose permission — edits ship UPSTREAM as a proposal (no autosave,
 * explicit action), then the editor resets to the pristine mirror. */
export type EditorMode = 'direct' | 'readonly' | 'propose'

export default function DocEditor({
  doc,
  mode = 'direct',
  onSaved,
  onProposed,
  onSelectionBlock,
  reviewMap,
}: {
  doc: EditableDoc
  mode?: EditorMode
  /** `docId` names the doc the save landed on: the unmount flush can fire
   * after the parent has moved to another doc, which must ignore it */
  onSaved: (epoch: number, docId: string) => void
  onProposed?: () => void
  onSelectionBlock?: (blockId: string | null) => void
  /** blocks under review → tone; painted as node decorations, no remount */
  reviewMap?: ReviewMap
}) {
  const selCb = useRef(onSelectionBlock)
  selCb.current = onSelectionBlock
  const [saveState, setSaveState] = useState<SaveState>('clean')
  const [epoch, setEpoch] = useState(doc.epoch)

  // baseline: per block, the round-tripped markdown (comparison form) + structure
  const initial = useMemo(() => {
    const nodes: PMNode[] = []
    const baseline: BaselineBlock[] = []
    for (const b of doc.blocks) {
      const parsed = parser.parse(b.content)
      if (!parsed || parsed.childCount === 0) continue
      const children: PMNode[] = []
      parsed.forEach((child) => {
        children.push(child.type.create({ ...child.attrs, blockId: b.id }, child.content, child.marks))
      })
      nodes.push(...children)
      baseline.push({
        id: b.id,
        content: nodesToMarkdown(serializer, schema, children),
        parent: b.parent_id,
        order_key: b.order_key,
      })
    }
    return { nodes, baseline }
  }, [doc.docId])

  const baselineRef = useRef(initial.baseline)
  const epochRef = useRef(doc.epoch)
  useEffect(() => {
    baselineRef.current = initial.baseline
    epochRef.current = doc.epoch
    setEpoch(doc.epoch)
    setSaveState('clean')
  }, [initial])

  const editor = useEditor(
    {
      extensions,
      editable: mode !== 'readonly',
      content: {
        type: 'doc',
        content: initial.nodes.map((n) => n.toJSON()),
      },
      onUpdate: () => setSaveState('dirty'),
      onSelectionUpdate: ({ editor }) => {
        const sel = editor.state.selection
        if (sel.empty) {
          selCb.current?.(null)
          return
        }
        try {
          selCb.current?.(sel.$from.node(1)?.attrs?.blockId ?? null)
        } catch {
          selCb.current?.(null)
        }
      },
    },
    [doc.docId],
  )

  // review marks follow the map from DocView; an empty map clears them
  useEffect(() => {
    if (!editor || editor.isDestroyed) return
    setReviewMap(editor, reviewMap ?? {})
  }, [editor, reviewMap])

  // extract entries (grouping consecutive nodes that share a blockId)
  const extractEntries = (): Entry[] => {
    if (!editor) return []
    const entries: Entry[] = []
    let run: { id: string; nodes: PMNode[] } | null = null
    const flushRun = () => {
      if (run) {
        entries.push({
          id: run.id,
          content: nodesToMarkdown(serializer, schema, run.nodes),
          level: run.nodes[0].type.name === 'heading' ? run.nodes[0].attrs.level : 0,
        })
        run = null
      }
    }
    editor.state.doc.forEach((node) => {
      if (node.type.name === 'paragraph' && node.content.size === 0 && !node.attrs.blockId) {
        return // empty unsaved paragraphs are not blocks
      }
      const id: string | null = node.attrs.blockId ?? null
      if (id && run && run.id === id) {
        run.nodes.push(node)
        return
      }
      flushRun()
      if (id) run = { id, nodes: [node] }
      else
        entries.push({
          id: null,
          content: nodesToMarkdown(serializer, schema, [node]),
          level: node.type.name === 'heading' ? node.attrs.level : 0,
        })
    })
    flushRun()
    return entries
  }

  const save = async () => {
    if (!editor || saveState === 'saving' || mode === 'readonly') return
    const entries = extractEntries()
    const assigned: string[] = []
    const ops = computeOps(baselineRef.current, entries, () => {
      const id = crypto.randomUUID()
      assigned.push(id)
      return id
    })
    if (ops.length === 0) {
      setSaveState('clean')
      return
    }
    if (mode === 'propose') {
      // pessimistic mirror: the edit becomes an upstream proposal; the local
      // doc never changes until the owner accepts and a pull lands it
      setSaveState('saving')
      try {
        await api('/admin/propose_upstream', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ doc_id: doc.docId, ops, note: '' }),
        })
        setSaveState('clean')
        onProposed?.()
      } catch (e) {
        setSaveState('dirty')
        notify(proposeErrorText(e), 'warn')
      }
      return
    }
    setSaveState('saving')
    try {
      const out = await api<{ epoch: number }>('/api/propose', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ doc_id: doc.docId, base_epoch: epochRef.current, ops }),
      })
      // stamp fresh ids onto the inserted nodes so the next diff sees them
      let cursor = 0
      const inserted = ops
        .filter((o) => o.kind.op === 'insert')
        .map((o) => o.kind as unknown as { block_id: string; content: string })
      if (editor && inserted.length) {
        const tr = editor.state.tr
        editor.state.doc.forEach((node, pos) => {
          if (!node.attrs.blockId && node.content.size > 0 && cursor < inserted.length) {
            tr.setNodeAttribute(pos, 'blockId', inserted[cursor].block_id)
            cursor++
          }
        })
        tr.setMeta('addToHistory', false)
        editor.view.dispatch(tr)
      }
      // new baseline = what we just wrote
      baselineRef.current = rebuildBaseline(baselineRef.current, entries, ops)
      epochRef.current = out.epoch
      setEpoch(out.epoch)
      setSaveState('clean')
      lastSaveError.current = null
      onSaved(out.epoch, doc.docId)
    } catch (e) {
      // the text is still in the editor and stays dirty; the retry clock
      // below keeps trying. Say so ONCE per distinct cause — a silent
      // failure here was how unsaved edits used to vanish.
      setSaveState('dirty')
      const msg = saveErrorText(e)
      if (lastSaveError.current !== msg) {
        lastSaveError.current = msg
        notify(msg, 'warn')
      }
      console.error(e)
    }
  }
  const lastSaveError = useRef<string | null>(null)

  // debounce autosave; flush on unmount/navigation
  const saveRef = useRef(save)
  saveRef.current = save
  const modeRef = useRef(mode)
  modeRef.current = mode

  // cold-editor heartbeat (auto-hot): while someone is actively in this
  // editor, tell the daemon — two concurrent editors escalate to a live
  // session (the DocView owns the escalation; we just ping)
  const editorKey = useRef(crypto.randomUUID())
  useEffect(() => {
    if (mode === 'readonly') return
    const t = setInterval(() => {
      if (!editor) return
      const active = editor.isFocused || saveState !== 'clean'
      if (!active) return
      api(`/api/doc/${doc.docId}/editing`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ key: editorKey.current }),
      }).catch(() => {})
    }, 4000)
    return () => clearInterval(t)
  }, [editor, mode, doc.docId, saveState])
  useEffect(() => {
    if (saveState !== 'dirty' || mode !== 'direct') return
    const t = setTimeout(() => saveRef.current(), 1200)
    return () => clearTimeout(t)
  }, [saveState, editor?.state.doc, mode])
  // a failed save leaves the doc dirty with no new keystrokes to re-arm the
  // debounce: retry on a slow clock until it lands (daemon back, live
  // session over, …)
  useEffect(() => {
    if (saveState !== 'dirty' || mode !== 'direct') return
    const t = setInterval(() => saveRef.current(), 5000)
    return () => clearInterval(t)
  }, [saveState, mode])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') saveRef.current()
    }
    window.addEventListener('keydown', onKey)
    return () => {
      window.removeEventListener('keydown', onKey)
      if (modeRef.current === 'direct') saveRef.current()
    }
  }, [])

  if (mode === 'propose') {
    return (
      <>
        <EditorContent editor={editor} />
        {saveState !== 'clean' ? (
          <button
            className="propose-cta"
            disabled={saveState === 'saving'}
            onClick={() => saveRef.current()}
          >
            {saveState === 'saving' ? 'sending…' : 'suggest changes ⌘⏎'}
          </button>
        ) : (
          <span className="save-state clean" title={`version ${epoch}`}>up to date</span>
        )}
      </>
    )
  }
  return (
    <>
      <EditorContent editor={editor} />
      {mode === 'direct' && (
        <span className={`save-state ${saveState}`}>
          {saveState === 'clean' ? 'saved' : saveState === 'dirty' ? 'unsaved' : 'saving…'}
        </span>
      )}
    </>
  )
}

/** After a successful save the written entries ARE the new baseline. */
export function rebuildBaseline(
  old: BaselineBlock[],
  entries: Entry[],
  ops: { kind: Record<string, unknown> & { op: string } }[],
): BaselineBlock[] {
  const oldById = new Map(old.map((b) => [b.id, b]))
  const inserts = ops.filter((o) => o.kind.op === 'insert')
  const moves = new Map(
    ops
      .filter((o) => o.kind.op === 'move')
      .map((o) => [o.kind.target as string, o.kind as unknown as { new_parent: string | null; new_order_key: string }]),
  )
  let insertCursor = 0
  const out: BaselineBlock[] = []
  for (const e of entries) {
    if (e.id) {
      const prev = oldById.get(e.id)
      const mv = moves.get(e.id)
      out.push({
        id: e.id,
        content: e.content,
        parent: mv ? mv.new_parent : (prev?.parent ?? null),
        order_key: mv ? mv.new_order_key : (prev?.order_key ?? 'i'),
      })
    } else {
      const ins = inserts[insertCursor++]?.kind as unknown as
        | { block_id: string; parent_id: string | null; order_key: string }
        | undefined
      if (ins)
        out.push({ id: ins.block_id, content: e.content, parent: ins.parent_id, order_key: ins.order_key })
    }
  }
  return out
}
