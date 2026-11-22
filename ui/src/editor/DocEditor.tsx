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
import { CodeHighlight } from './CodeHighlight'
import { ReviewHighlight, ReviewMap, ReviewExtras, setReviewMap } from './ReviewHighlight'
import type { Node as PMNode } from '@tiptap/pm/model'
import type { Transaction } from '@tiptap/pm/state'
import { api, Block } from '../types'
import { withCsrf } from '../auth'
import { notify } from '../Notice'
import { isPermanentSaveError, permanentSaveErrorText, saveErrorText } from '../hints'
import { BaselineBlock, Entry, HiddenBlock, Op, computeOps } from './diff'
import { debounce } from '../timing'
import { makeParser, makeSerializer, nodesToMarkdown } from './markdown'
import { AfterSave, PendingRequest, afterSave, requestFor } from './savePlan'
import { registerEditor } from './registry'
import './editor.css'

/** transaction meta marking DocEditor's own post-save blockId stamping, so
 * onUpdate can tell it apart from a real edit. */
const STAMP_META = 'taisce-stamp'

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

/** The editor extension set. */
export function makeExtensions() {
  return [
    StarterKit.configure({
      link: { openOnClick: false },
      codeBlock: false,
    }),
    CodeBlock.extend({
      addNodeView() {
        return ReactNodeViewRenderer(CodeBlockView)
      },
    }),
    TableKit.configure({ table: { resizable: false } }),
    WikilinkDeco,
    CodeHighlight,
    BlockId,
    ReviewHighlight,
  ]
}
export const extensions = makeExtensions()
export const schema = getSchema(extensions)
export const parser = makeParser(schema)
export const serializer = makeSerializer()

export interface EditableDoc {
  docId: string
  epoch: number
  /** pre-order flattened live blocks, comments and frontmatter excluded */
  blocks: Block[]
  /** the blocks left out (comments, frontmatter, old canvases), in place */
  hidden?: HiddenBlock[]
}

/** Where the cursor was when an editor unmounted, per doc: a remount of the
 * same doc (fresh content from the server) puts it back. */
const selectionMemo = new Map<string, { anchor: number; head: number; focused: boolean }>()

/** True when a save's answer is the very next epoch after its base: nobody
 * else wrote in between, so the editor shows everything the doc holds. A
 * jump past base + 1 means the daemon rebased the save over someone else's
 * edits (which this editor does not show); no jump means nothing applied. */
export function isOwnEpoch(base: number, epoch: number): boolean {
  return epoch === base + 1
}

type SaveState = 'clean' | 'dirty' | 'saving'
/** What DocView's save indicator shows: `failed` = dirty after a save error. */
export type EditorSaveState = SaveState | 'failed'

export default function DocEditor({
  doc,
  onSaved,
  onSelectionBlock,
  reviewMap,
  reviewExtras,
  editable = true,
  onSaveState,
  restoreSelection = false,
}: {
  doc: EditableDoc
  /** `docId` names the doc the save landed on: the unmount flush can fire
   * after the parent has moved to another doc, which must ignore it. `own`
   * is false when the save landed over someone else's edits (isOwnEpoch):
   * the doc holds more than this editor shows */
  onSaved: (epoch: number, docId: string, own: boolean) => void
  onSelectionBlock?: (blockId: string | null) => void
  /** blocks under review → tone; painted as node decorations, no remount */
  reviewMap?: ReviewMap
  /** the verdict bars, inline diffs and comment bubbles that ride on the map */
  reviewExtras?: ReviewExtras
  /** false for a viewer of a shared workspace: no typing, no saves */
  editable?: boolean
  /** every save-state change, for the doc header's indicator */
  onSaveState?: (s: EditorSaveState) => void
  /** a remount of the same doc: put the cursor back where it was */
  restoreSelection?: boolean
}) {
  const selCb = useRef(onSelectionBlock)
  selCb.current = onSelectionBlock
  const [saveState, setSaveStateRaw] = useState<SaveState>('clean')
  const [failed, setFailedRaw] = useState(false)
  // the same, readable synchronously (flush awaits a save and then asks)
  const stateRef = useRef<SaveState>('clean')
  const failedRef = useRef(false)
  const setSaveState = (s: SaveState) => {
    stateRef.current = s
    setSaveStateRaw(s)
  }
  const setFailed = (f: boolean) => {
    failedRef.current = f
    setFailedRaw(f)
  }
  /** a save the daemon refused for good: no retry clock until the next edit */
  const [stopped, setStopped] = useState(false)
  /** the request being retried, so a retry carries the same request_id */
  const pending = useRef<PendingRequest | null>(null)
  const stateCb = useRef(onSaveState)
  stateCb.current = onSaveState
  useEffect(() => {
    stateCb.current?.(saveState === 'dirty' && failed ? 'failed' : saveState)
  }, [saveState, failed])
  const docIdRef = useRef(doc.docId)
  docIdRef.current = doc.docId
  const editableRef = useRef(editable)
  editableRef.current = editable
  // autosave debounce, driven by the editor's update event: every keystroke
  // re-arms it, so the save lands 1.2s after the LAST edit. (An effect keyed
  // on editor.state.doc did not re-arm — Tiptap 3 does not re-render per
  // transaction — so the timer fired 1.2s after the FIRST keystroke.)
  const saveRef = useRef<() => Promise<void>>(async () => {})
  const autosave = useRef(debounce(1200, () => saveRef.current()))
  /** the save currently in flight, or null; the single concurrency guard */
  const inFlight = useRef<Promise<void> | null>(null)
  /** false once the cleanup below has run — a save that resolves after that
   * must not re-arm timers that no longer exist */
  const mounted = useRef(true)
  const lastPlan = useRef<AfterSave>('clean')

  // baseline: per block, the round-tripped markdown (comparison form) + structure
  const initial = useMemo(() => loadDoc(doc), [doc.docId])

  const baselineRef = useRef(initial.baseline)
  const epochRef = useRef(doc.epoch)
  useEffect(() => {
    baselineRef.current = initial.baseline
    epochRef.current = doc.epoch
    setSaveState('clean')
  }, [initial])

  const editor = useEditor(
    {
      extensions,
      editable,
      content: {
        type: 'doc',
        content: initial.nodes.map((n) => n.toJSON()),
      },
      onUpdate: ({ transaction }) => {
        // the post-save blockId stamp is our own bookkeeping, not an edit:
        // arming the debounce for it caused a spurious empty save 1.2s later
        if (transaction.getMeta(STAMP_META)) return
        setStopped(false)
        setSaveState('dirty')
        autosave.current.arm()
      },
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

  const editorRef = useRef(editor)
  editorRef.current = editor
  // a remount with fresh content keeps the cursor (clamped to the new doc)
  useEffect(() => {
    if (!editor || editor.isDestroyed || !restoreSelection) return
    const m = selectionMemo.get(doc.docId)
    selectionMemo.delete(doc.docId)
    if (!m) return
    const size = editor.state.doc.content.size
    const clamp = (n: number) => Math.max(0, Math.min(n, size))
    try {
      editor.commands.setTextSelection({ from: clamp(m.anchor), to: clamp(m.head) })
      if (m.focused) editor.commands.focus(undefined, { scrollIntoView: false })
    } catch {
      // a position the new doc cannot hold: leave the cursor at the start
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editor])

  // review marks follow the map from DocView; an empty map clears them
  useEffect(() => {
    if (!editor || editor.isDestroyed) return
    setReviewMap(editor, reviewMap ?? {}, reviewExtras ?? {})
  }, [editor, reviewMap, reviewExtras])

  useEffect(() => {
    if (!editor || editor.isDestroyed) return
    if (editor.isEditable !== editable) editor.setEditable(editable, false)
  }, [editor, editable])

  /** Plan a save of what the editor holds now: every new block gets its id
   * here, before the diff, and the id goes on its node right away. The
   * insert carries it, and an edit made while the request is in flight moves
   * the node with its id (never pair inserts with id-less nodes in order —
   * computeOps groups inserts by parent, so that pairing gave blocks each
   * other's ids). A failed save keeps them, so its retry sends the same
   * inserts — under the same request_id. */
  const prepare = () => {
    if (!editor) return null
    const { entries, ops, stamps } = planSave(editor.state.doc, baselineRef.current, doc.hidden ?? [])
    if (stamps.length && !editor.isDestroyed) {
      const tr = editor.state.tr
      applyStamps(tr, stamps)
      tr.setMeta('addToHistory', false)
      tr.setMeta(STAMP_META, true)
      editor.view.dispatch(tr)
    }
    const base = epochRef.current
    if (ops.length === 0) return { entries, ops, base, req: null }
    const req = requestFor(pending.current, JSON.stringify({ doc_id: doc.docId, base_epoch: base, ops }))
    pending.current = req
    return { entries, ops, base, req }
  }

  const runSave = async () => {
    if (!editor || !editableRef.current) return
    autosave.current.cancel()
    const { entries, ops, base, req } = prepare()!
    // what this save writes; edits landing while the request is in flight
    // make the doc differ from it, and the editor must stay dirty for them
    const snapshot = editor.state.doc
    if (!req) {
      setSaveState('clean')
      return
    }
    setSaveState('saving')
    try {
      const out = await api<{ epoch: number }>('/api/propose', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...JSON.parse(req.body), request_id: req.id }),
      })
      pending.current = null
      const editedMeanwhile = !editor.state.doc.eq(snapshot)
      // new baseline = what we just wrote
      baselineRef.current = rebuildBaseline(baselineRef.current, entries, ops)
      epochRef.current = out.epoch
      lastSaveError.current = null
      setFailed(false)
      setStopped(false)
      // typed during the request? those keystrokes are not in what landed
      const plan = afterSave({ editedMeanwhile, mounted: mounted.current })
      lastPlan.current = plan
      if (plan === 'clean') {
        setSaveState('clean')
      } else {
        setSaveState('dirty')
        // 'save-now' (typed then switched doc) is driven by the unmount
        // cleanup below — no debounce or retry clock survives the unmount
        if (plan === 'rearm') autosave.current.arm()
      }
      onSaved(out.epoch, doc.docId, isOwnEpoch(base, out.epoch))
    } catch (e) {
      // the text is still in the editor and stays dirty; the retry clock
      // below keeps trying. Say so ONCE per distinct cause — a silent
      // failure here was how unsaved edits used to vanish.
      setSaveState('dirty')
      setFailed(true)
      // refused for good (doc gone, not writable, invalid ops): retrying
      // every 5 s forever only repeats the refusal
      const permanent = isPermanentSaveError(e)
      if (permanent) setStopped(true)
      const msg = permanent ? permanentSaveErrorText(e) : saveErrorText(e)
      if (lastSaveError.current !== msg) {
        lastSaveError.current = msg
        notify(msg, 'warn')
      }
      console.error(e)
    }
  }
  const lastSaveError = useRef<string | null>(null)

  /** One save at a time, tracked by promise rather than React state: the
   * unmount cleanup runs after the component is gone, where `saveState` is
   * stale, and it must be able to await the request in flight. */
  const save = (): Promise<void> => {
    if (inFlight.current) return inFlight.current
    const p = runSave().finally(() => {
      inFlight.current = null
    })
    inFlight.current = p
    return p
  }

  saveRef.current = save

  /** Save now and wait: true once nothing is unsaved. Keystrokes typed while
   * a save is in flight need one more, so it tries a few times. */
  const flush = async (): Promise<boolean> => {
    if (!editableRef.current) return true
    for (let i = 0; i < 3; i++) {
      autosave.current.cancel()
      if (inFlight.current) await inFlight.current
      await saveRef.current()
      if (stateRef.current === 'clean') return true
      if (failedRef.current) return false
    }
    return stateRef.current === 'clean'
  }
  const flushRef = useRef(flush)
  flushRef.current = flush

  // closing or reloading the tab: unsaved typing would be lost with the page
  // (the debounce is 1.2 s). Ask first where the browser lets a page ask,
  // and as the page goes, send what is unsaved with a keepalive request that
  // outlives it. The same request_id as any later save of the same ops, so
  // if the page stays after all (bfcache) nothing is applied twice. A save in
  // flight is left to land: re-sending its ops would duplicate them.
  const keepaliveRef = useRef<() => void>(() => {})
  keepaliveRef.current = () => {
    if (stateRef.current === 'clean' || inFlight.current || !editableRef.current || !editor) return
    const plan = prepare()
    if (!plan?.req) return
    try {
      void fetch(
        '/api/propose',
        withCsrf({
          method: 'POST',
          keepalive: true,
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ ...JSON.parse(plan.req.body), request_id: plan.req.id }),
        }),
      ).catch(() => {})
    } catch {
      // no fetch (or keepalive refused, e.g. too large): the prompt was the guard
    }
  }
  useEffect(() => {
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      if (stateRef.current === 'clean' || !editableRef.current) return
      e.preventDefault()
      e.returnValue = ''
    }
    const onPageHide = () => keepaliveRef.current()
    window.addEventListener('beforeunload', onBeforeUnload)
    window.addEventListener('pagehide', onPageHide)
    return () => {
      window.removeEventListener('beforeunload', onBeforeUnload)
      window.removeEventListener('pagehide', onPageHide)
    }
  }, [])

  useEffect(
    () =>
      registerEditor(doc.docId, {
        flush: () => flushRef.current(),
        catchUp: (blocks, epoch) => {
          if (stateRef.current !== 'clean' || inFlight.current) return false
          if (!sameBaseline(loadDoc({ docId: doc.docId, epoch, blocks }).baseline, baselineRef.current)) return false
          epochRef.current = Math.max(epochRef.current, epoch)
          return true
        },
      }),
    [doc.docId],
  )

  // a failed save leaves the doc dirty with no new keystrokes to re-arm the
  // debounce: retry on a slow clock until it lands (daemon back, …)
  useEffect(() => {
    if (saveState !== 'dirty' || stopped) return
    const t = setInterval(() => saveRef.current(), 5000)
    return () => clearInterval(t)
  }, [saveState, stopped])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') saveRef.current()
    }
    window.addEventListener('keydown', onKey)
    const pending = autosave.current
    return () => {
      window.removeEventListener('keydown', onKey)
      try {
        const ed = editorRef.current
        if (ed) {
          const sel = ed.state.selection
          selectionMemo.set(docIdRef.current, { anchor: sel.anchor, head: sel.head, focused: ed.isFocused })
        }
      } catch {
        // the editor is already gone: nothing to remember
      }
      // unmount/navigation: land whatever is pending now (the save reports
      // its own docId, so a parent that has moved on ignores the result)
      pending.cancel()
      mounted.current = false
      const flight = inFlight.current
      if (!flight) {
        saveRef.current()
        return
      }
      // a save is mid-request: its response cannot carry keystrokes typed
      // after it started, so wait for it and save those once more
      flight.then(() => {
        if (lastPlan.current === 'save-now') saveRef.current()
      })
    }
  }, [])

  return (
    <>
      <EditorContent editor={editor} />
      {/* App's deploy reload looks for `.save-state.dirty|saving` before it
          reloads over unsaved work; the visible indicator is DocView's */}
      <span className={`save-state ${saveState}`} hidden>
        {saveState === 'clean' ? 'saved' : saveState === 'dirty' ? 'unsaved' : 'saving…'}
      </span>
    </>
  )
}

/** The editor's nodes (each carrying its block id) and the baseline
 * (per block, the round-tripped markdown + structure) for a doc. */
export function loadDoc(doc: EditableDoc): { nodes: PMNode[]; baseline: BaselineBlock[] } {
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
}

/** The editor doc's top-level entries for the diff (consecutive nodes
 * sharing a blockId are one block; empty unsaved paragraphs are none). */
export function entriesOf(doc: PMNode, positions?: number[]): Entry[] {
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
  doc.forEach((node, pos) => {
    if (node.type.name === 'paragraph' && node.content.size === 0 && !node.attrs.blockId) {
      return // empty unsaved paragraphs are not blocks
    }
    const id: string | null = node.attrs.blockId ?? null
    if (id && run && run.id === id) {
      run.nodes.push(node)
      return
    }
    flushRun()
    // where the entry's first node sits (entries and positions stay parallel)
    positions?.push(pos)
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

/** Two baselines hold the same blocks in the same places. */
export function sameBaseline(a: BaselineBlock[], b: BaselineBlock[]): boolean {
  return (
    a.length === b.length &&
    a.every((x, i) => x.id === b[i].id && x.content === b[i].content && x.parent === b[i].parent && x.order_key === b[i].order_key)
  )
}

/** A new block's id and the node it belongs on: `pos` is where that node
 * started in the doc the save was planned from. */
export interface Stamp {
  pos: number
  id: string
  type: string
}

/** What one save writes: the entries (every new one already given its id),
 * the ops, and the stamps that put those ids on their nodes once it lands. */
export function planSave(
  doc: PMNode,
  baseline: BaselineBlock[],
  hidden: HiddenBlock[],
  newId: () => string = () => crypto.randomUUID(),
): { entries: Entry[]; ops: Op[]; stamps: Stamp[] } {
  const positions: number[] = []
  const stamps: Stamp[] = []
  const entries = entriesOf(doc, positions).map((e, i) => {
    if (e.id) return e
    const id = newId()
    stamps.push({ pos: positions[i], id, type: doc.nodeAt(positions[i])!.type.name })
    return { ...e, id }
  })
  const ops = computeOps(baseline, entries, newId, undefined, hidden)
  return { entries, ops, stamps }
}

/** Put the new ids on their nodes, in the doc the save was planned from (a
 * position that no longer holds an id-less top-level node of that type is
 * left alone). True when anything was set. */
export function applyStamps(tr: Transaction, stamps: Stamp[]): boolean {
  let any = false
  for (const s of stamps) {
    if (s.pos >= tr.doc.content.size || tr.doc.resolve(s.pos).depth !== 0) continue
    const node = tr.doc.nodeAt(s.pos)
    if (!node || node.type.name !== s.type || node.attrs.blockId) continue
    tr.setNodeAttribute(s.pos, 'blockId', s.id)
    any = true
  }
  return any
}

/** After a successful save the written entries ARE the new baseline. */
export function rebuildBaseline(
  old: BaselineBlock[],
  entries: Entry[],
  ops: { kind: Record<string, unknown> & { op: string } }[],
): BaselineBlock[] {
  const oldById = new Map(old.map((b) => [b.id, b]))
  const inserts = ops.filter((o) => o.kind.op === 'insert')
  type Ins = { block_id: string; parent_id: string | null; order_key: string }
  // an entry that carries its insert's id takes that insert's place, whatever
  // order computeOps emitted the inserts in (it groups them by parent)
  const insertById = new Map(inserts.map((o) => [o.kind.block_id as string, o.kind as unknown as Ins]))
  const claimed = new Set<string>()
  for (const e of entries) if (e.id && insertById.has(e.id)) claimed.add(e.id)
  const unclaimed = inserts.filter((o) => !claimed.has(o.kind.block_id as string))
  const moves = new Map(
    ops
      .filter((o) => o.kind.op === 'move')
      .map((o) => [o.kind.target as string, o.kind as unknown as { new_parent: string | null; new_order_key: string }]),
  )
  let insertCursor = 0
  const out: BaselineBlock[] = []
  for (const e of entries) {
    const ins = e.id ? insertById.get(e.id) : undefined
    if (ins && !oldById.has(e.id!)) {
      out.push({ id: ins.block_id, content: e.content, parent: ins.parent_id, order_key: ins.order_key })
    } else if (e.id) {
      const prev = oldById.get(e.id)
      const mv = moves.get(e.id)
      out.push({
        id: e.id,
        content: e.content,
        parent: mv ? mv.new_parent : (prev?.parent ?? null),
        order_key: mv ? mv.new_order_key : (prev?.order_key ?? 'i'),
      })
    } else {
      // id-less entries (callers that leave the ids to computeOps) pair with
      // the remaining inserts in order: right only when those share a parent
      const ins = unclaimed[insertCursor++]?.kind as unknown as Ins | undefined
      if (ins)
        out.push({ id: ins.block_id, content: e.content, parent: ins.parent_id, order_key: ins.order_key })
    }
  }
  return out
}
