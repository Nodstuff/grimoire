// The doc view: title, the head chips, the editor, side panels (history,
// comments, tend, share, review rail) and backlinks. Moved out of App.tsx
// unchanged for the Lapis shell; stage 2 rebuilds it to the Doc artboard.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import DocEditor from '../editor/DocEditor'
import { splitTree } from '../editor/splitTree'
import TendPanel from '../TendPanel'
import LivingChip from '../LivingChip'
import ReviewRail from '../ReviewRail'
import SharePanel from '../SharePanel'
import { notify, errText } from '../Notice'
import { buildHighlightMap } from '../review'
import { api, Block, Doc, DocTree, HistoryRow, QueueRow, SearchHit } from '../types'
import type { OpenDoc } from '../App'

/** The doc side panels, owned by App so a doc switch does not close one. */
export type Panel = 'none' | 'history' | 'comments' | 'tend' | 'review' | 'share'

/** Click-to-rename doc title. The server rewrites inbound [[wikilinks]] to
 * the new title in docs the renamer can write (`rename_doc`). */
function DocTitle({ doc, onRenamed }: { doc: Doc; onRenamed: () => void }) {
  const [editing, setEditing] = useState(false)
  const [value, setValue] = useState(doc.title)
  // Enter unmounts the input, whose blur fires save again: one rename only
  const saving = useRef(false)

  const save = async () => {
    if (saving.current) return
    saving.current = true
    setEditing(false)
    const title = value.trim()
    if (!title || title === doc.title) {
      setValue(doc.title)
      saving.current = false
      return
    }
    try {
      await api(`/api/doc/${doc.id}/rename`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ title }),
      })
    } catch (e) {
      console.error(e)
      notify(`rename failed: ${errText(e)}`, 'warn')
      setValue(doc.title)
    } finally {
      saving.current = false
    }
    onRenamed()
  }

  if (!editing)
    return (
      <h1 className="doc-title" title="click to rename" onClick={() => {
        setValue(doc.title)
        setEditing(true)
      }}>
        {doc.title}
      </h1>
    )
  return (
    <input
      className="doc-title-edit"
      autoFocus
      value={value}
      onChange={(e) => setValue(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === 'Enter') save()
        if (e.key === 'Escape') {
          setValue(doc.title)
          setEditing(false)
        }
      }}
      onBlur={save}
    />
  )
}

export default function DocView({
  docId,
  onOpenDoc,
  docs,
  dataVersion,
  anchor,
  reviewIntent = false,
  panel,
  setPanel,
  signedIn = false,
}: {
  docId: string
  onOpenDoc: OpenDoc
  docs: Doc[]
  dataVersion: number
  anchor?: string | null
  /** opened from the review queue: open the rail on load */
  reviewIntent?: boolean
  /** owned by App so it outlives this component's per-doc remount */
  panel: Panel
  setPanel: (p: Panel) => void
  /** SERVER mode, signed in through this browser: share links are offered */
  signedIn?: boolean
}) {
  const [tree, setTree] = useState<DocTree | null>(null)
  const [backlinks, setBacklinks] = useState<SearchHit[]>([])
  // stale guard: DocView is keyed by docId (one instance per open doc), so a
  // fetch that resolves after unmount — or after a doc switch — is for a doc
  // that is no longer on screen. `gen` bumps on every docId change too, in
  // case a caller ever reuses the instance.
  const gen = useRef(0)
  useEffect(() => {
    const g = ++gen.current
    return () => {
      if (gen.current === g) gen.current++
    }
  }, [docId])
  const fresh = useCallback(
    (g: number) => gen.current === g,
    [],
  )
  // open review items for THIS doc (yellow = applied+flagged, red = parked)
  const [reviewItems, setReviewItems] = useState<QueueRow[]>([])
  const loadReview = useCallback(() => {
    const g = gen.current
    api<QueueRow[]>(`/api/doc/${docId}/review`)
      .then((r) => fresh(g) && setReviewItems(Array.isArray(r) ? r : []))
      .catch(() => fresh(g) && setReviewItems([]))
  }, [docId, fresh])
  const [selBlock, setSelBlock] = useState<string | null>(null)
  const [selRect, setSelRect] = useState<{ x: number; y: number } | null>(null)
  const [commentTarget, setCommentTarget] = useState<string | null>(null)
  // epochs this editor produced itself — external changes are anything above
  const ownEpoch = useRef(0)
  const [editorGen, setEditorGen] = useState(0)

  // anchor the comment bubble just above the selected text
  const onSelectionBlock = useCallback((blockId: string | null) => {
    setSelBlock(blockId)
    if (!blockId) {
      setSelRect(null)
      return
    }
    requestAnimationFrame(() => {
      const sel = window.getSelection()
      if (!sel || sel.rangeCount === 0 || sel.isCollapsed) {
        setSelRect(null)
        return
      }
      const r = sel.getRangeAt(0).getBoundingClientRect()
      setSelRect({ x: r.left + r.width / 2, y: r.top })
    })
  }, [])

  const loadTree = useCallback(() => {
    const g = gen.current
    api<DocTree>(`/api/doc/${docId}`)
      .then((t) => fresh(g) && setTree(t))
      .catch(console.error)
    api<SearchHit[]>(`/api/doc/${docId}/backlinks`)
      .then((b) => fresh(g) && setBacklinks(b))
      .catch(() => fresh(g) && setBacklinks([]))
  }, [docId, fresh])

  useEffect(() => {
    setTree(null)
    // the panel choice is the user's, not the doc's: only a review-queue
    // open overrides it (the same rule as the reviewIntent effect below)
    if (reviewIntent) setPanel('review')
    setCommentTarget(null)
    setReviewItems([])
    ownEpoch.current = 0
    loadTree()
    loadReview()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadTree, loadReview])

  // a later { review: true } open of the SAME doc still opens the rail
  useEffect(() => {
    if (reviewIntent) setPanel('review')
  }, [reviewIntent, anchor])

  useEffect(() => {
    if (dataVersion === 0) return
    loadReview()
  }, [dataVersion, loadReview])

  // after a resolve: content may have moved (yellow decline reverts, red
  // accept applies) — refetch, remount the editor if the epoch moved
  const treeRef = useRef<DocTree | null>(null)
  treeRef.current = tree
  const afterResolve = useCallback(() => {
    loadReview()
    const g = gen.current
    api<DocTree>(`/api/doc/${docId}`)
      .then((next) => {
        if (!fresh(g)) return
        const cur = treeRef.current
        if (!cur || next.doc.current_epoch !== cur.doc.current_epoch) {
          ownEpoch.current = Math.max(ownEpoch.current, next.doc.current_epoch)
          setEditorGen((g) => g + 1)
        }
        setTree(next)
      })
      .catch(() => {})
  }, [docId, loadReview, fresh])

  const highlightMap = useMemo(() => buildHighlightMap(reviewItems), [reviewItems])

  // live refresh: when the store changed and this doc moved past what our own
  // saves produced, reload it and remount the editor with the fresh content
  // (skipped while dirty — the pending autosave lands first, next tick catches up)
  const refreshFromStore = useCallback(() => {
    const cur = treeRef.current
    if (!cur) return
    const g = gen.current
    api<DocTree>(`/api/doc/${docId}`)
      .then((next) => {
        if (!fresh(g)) return
        const known = Math.max(cur.doc.current_epoch, ownEpoch.current)
        const dirty = document.querySelector('.save-state.dirty, .save-state.saving')
        if (next.doc.current_epoch > known && !dirty) {
          setTree(next)
          setEditorGen((g) => g + 1)
        } else if (next.doc.status !== cur.doc.status) {
          setTree(next)
        }
        api<SearchHit[]>(`/api/doc/${docId}/backlinks`)
          .then((b) => fresh(g) && setBacklinks(b))
          .catch(() => {})
      })
      .catch((e) => console.warn('doc refresh failed', docId, e))
  }, [docId, fresh])
  useEffect(() => {
    if (dataVersion === 0) return
    refreshFromStore()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dataVersion])

  const byTitle = useMemo(() => new Map(docs.map((d) => [d.title, d.id])), [docs])

  // anchor from a link: [[Doc#^block-uuid]] finds the block by stable id,
  // [[Doc#Heading]] falls back to text match. Scroll + flash.
  useEffect(() => {
    if (!anchor || !tree) return
    // retry across editor remounts (live refresh can race the first attempt)
    const attempt = () => {
      let el: Element | null = null
      if (anchor.startsWith('^')) {
        el = document.querySelector(`[data-block-id="${anchor.slice(1)}"]`)
      }
      if (!el) {
        const needle = anchor.replace(/^\^/, '').toLowerCase()
        el =
          [...document.querySelectorAll(
            '.ProseMirror h1, .ProseMirror h2, .ProseMirror h3, .ProseMirror h4, .ProseMirror p',
          )].find((n) => n.textContent?.toLowerCase().includes(needle)) ?? null
      }
      if (el) {
        el.scrollIntoView({ block: 'start' })
        el.classList.add('anchor-flash')
        setTimeout(() => el.classList.remove('anchor-flash'), 1600)
        return true
      }
      return false
    }
    const timers = [250, 900, 1800].map((ms) => setTimeout(attempt, ms))
    return () => timers.forEach(clearTimeout)
  }, [anchor, tree])

  // wikilink click-through: decorated targets carry data-target
  const onStageClick = useCallback(
    (e: React.MouseEvent) => {
      const el = (e.target as HTMLElement).closest('.wl-target') as HTMLElement | null
      if (!el) return
      const target = el.dataset.target ?? ''
      const [path, fragment] = target.split('#')
      const name = path.split('/').pop()?.trim() ?? path
      const id = byTitle.get(name)
      if (id) {
        e.preventDefault()
        onOpenDoc(id, fragment?.trim())
      }
    },
    [byTitle, onOpenDoc],
  )

  const { editable, comments, allBlocks, removedCanvases } = useMemo(() => {
    if (!tree)
      return {
        editable: null,
        comments: [] as Block[],
        allBlocks: [] as Block[],
        removedCanvases: 0,
      }
    const { blocks, comments, allBlocks, hidden, removedCanvases } = splitTree(tree.roots)
    return {
      editable: { docId, epoch: tree.doc.current_epoch, blocks, hidden },
      comments,
      allBlocks,
      removedCanvases,
    }
  }, [tree, docId])

  if (!tree || !editable) return <div className="empty">…</div>

  return (
    <article className="doc" onClick={onStageClick}>
      <div className="doc-head">
        <span className="head-actions">
          {tree.doc.review_policy && <span className="meta policy">{tree.doc.review_policy}</span>}
          <LivingChip docId={docId} dataVersion={dataVersion} />
          <button
            className={`chip ${panel === 'history' ? 'on' : ''}`}
            onClick={() => setPanel(panel === 'history' ? 'none' : 'history')}
          >
            history
          </button>
          <button
            className={`chip ${panel === 'comments' ? 'on' : ''}`}
            onClick={() => setPanel(panel === 'comments' ? 'none' : 'comments')}
          >
            comments{comments.length > 0 ? ` ${comments.length}` : ''}
          </button>
          <button
            className={`chip ${panel === 'tend' ? 'on' : ''} ${docs.find((d) => d.id === docId)?.is_tended ? 'tended' : ''}`}
            onClick={() => setPanel(panel === 'tend' ? 'none' : 'tend')}
          >
            {docs.find((d) => d.id === docId)?.is_tended ? '🌿 tended' : 'tend'}
          </button>
          {signedIn && (
            <button
              className={`chip ${panel === 'share' ? 'on' : ''}`}
              title="a public, read-only link to a snapshot of this doc"
              onClick={() => setPanel(panel === 'share' ? 'none' : 'share')}
            >
              share link…
            </button>
          )}
          {reviewItems.length > 0 && (
            <button
              className={`chip review-chip ${panel === 'review' ? 'on' : ''}`}
              title="review changes in this doc"
              onClick={() => setPanel(panel === 'review' ? 'none' : 'review')}
            >
              ⚠ {reviewItems.length} to review
            </button>
          )}
        </span>
        <DocTitle doc={tree.doc} onRenamed={loadTree} />
      </div>
      {removedCanvases > 0 && (
        <div className="meta canvas-removed">
          Canvas (removed) · this doc holds {removedCanvases === 1 ? 'a canvas' : `${removedCanvases} canvases`} the
          app no longer shows
        </div>
      )}
      <DocEditor
        key={`${docId}:${editorGen}`}
        doc={editable}
        reviewMap={highlightMap}
        onSaved={(e, savedDocId) => {
          // the unmount flush of a PREVIOUS doc's editor reports its own id
          if (savedDocId !== docId) return
          ownEpoch.current = Math.max(ownEpoch.current, e)
        }}
        onSelectionBlock={onSelectionBlock}
      />
      {selBlock && selRect && panel !== 'comments' && (
        <span className="sel-actions" style={{ left: selRect.x, top: selRect.y }}>
          <button
            title="comment on selection"
            onMouseDown={(e) => {
              e.preventDefault()
              setCommentTarget(selBlock)
              setPanel('comments')
              setSelRect(null)
            }}
          >
            💬
          </button>
          <button
            title="copy block link"
            onMouseDown={(e) => {
              e.preventDefault()
              navigator.clipboard
                .writeText(`[[${tree.doc.title}#^${selBlock}]]`)
                .catch(() => {})
              setSelRect(null)
            }}
          >
            🔗
          </button>
        </span>
      )}
      {panel === 'history' && <HistoryPanel docId={docId} onClose={() => setPanel('none')} />}
      {panel === 'share' && signedIn && (
        <SharePanel docId={docId} title={tree.doc.title} onClose={() => setPanel('none')} />
      )}
      {panel === 'review' && (
        <ReviewRail items={reviewItems} onChanged={afterResolve} onClose={() => setPanel('none')} />
      )}
      {panel === 'tend' && (
        <TendPanel doc={tree.doc} onClose={() => setPanel('none')} dataVersion={dataVersion} />
      )}
      {panel === 'comments' && (
        <CommentsPanel
          comments={comments}
          allBlocks={allBlocks}
          target={commentTarget}
          setTarget={setCommentTarget}
          onPosted={loadTree}
          onClose={() => setPanel('none')}
        />
      )}
      {backlinks.length > 0 && (
        <div className="backlinks">
          <span className="meta">linked from</span>
          {backlinks.map((b) => (
            <span key={b.block.id} className="backlink" onClick={() => onOpenDoc(b.block.doc_id)}>
              {b.doc_title}
            </span>
          ))}
        </div>
      )}
    </article>
  )
}

/* ---------- provenance & comments panels (5.4 / 5.5) ---------- */

function opSnippet(kind: Record<string, unknown> & { op: string }): string {
  const c = typeof kind.content === 'string' ? (kind.content as string) : ''
  return c ? c.split('\n')[0].slice(0, 90) : `${kind.op} ${String(kind.target ?? '').slice(0, 8)}`
}

function HistoryPanel({ docId, onClose }: { docId: string; onClose: () => void }) {
  const [rows, setRows] = useState<HistoryRow[]>([])
  useEffect(() => {
    api<HistoryRow[]>(`/api/doc/${docId}/history`).then(setRows).catch(console.error)
  }, [docId])

  // one entry per epoch (a save/run), ops grouped beneath
  const epochs = useMemo(() => {
    const m = new Map<number, HistoryRow[]>()
    for (const r of rows) {
      const e = r.op.epoch_applied ?? -1
      if (!m.has(e)) m.set(e, [])
      m.get(e)!.push(r)
    }
    return [...m.entries()].sort((a, b) => b[0] - a[0])
  }, [rows])

  return (
    <aside className="panel">
      <div className="panel-head">
        <span>history</span>
        <button onClick={onClose}>esc</button>
      </div>
      {epochs.map(([epoch, ops]) => (
        <div key={epoch} className="epoch-group">
          <div className="epoch-line">
            <span className="meta">epoch {epoch}</span>
            <span className={`who ${ops[0].principal_kind}`}>{ops[0].principal_name}</span>
            {ops[0].op.verdict && ops[0].op.verdict !== 'green' && (
              <span className={`verdict v-${ops[0].op.verdict}`}>{ops[0].op.verdict}</span>
            )}
          </div>
          {ops.map((r) => (
            <div key={r.op.id} className="op-line">
              <span className="op-type">{r.op.kind.op}</span>
              <span className="op-snippet">{opSnippet(r.op.kind)}</span>
            </div>
          ))}
          {ops[0].op.source_refs.length > 0 && (
            <div className="refs">{ops[0].op.source_refs.join(' · ')}</div>
          )}
        </div>
      ))}
      {rows.length === 0 && <div className="palette-empty">no history</div>}
    </aside>
  )
}

function CommentsPanel({
  comments,
  allBlocks,
  target,
  setTarget,
  onPosted,
  onClose,
}: {
  comments: Block[]
  allBlocks: Block[]
  target: string | null
  setTarget: (t: string | null) => void
  onPosted: () => void
  onClose: () => void
}) {
  const [text, setText] = useState('')
  const [replyTo, setReplyTo] = useState<Block | null>(null)
  const byId = useMemo(() => new Map(allBlocks.map((b) => [b.id, b])), [allBlocks])

  const roots = comments.filter((c) => !c.parent_id || !byId.get(c.parent_id)?.refers_to)
  const repliesOf = (id: string) => comments.filter((c) => c.parent_id === id)

  const post = async () => {
    const blockId = replyTo ? replyTo.refers_to : target
    if (!text.trim() || !blockId) return
    await api('/api/comment', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ block_id: blockId, text: text.trim(), reply_to: replyTo?.id ?? null }),
    }).catch((e) => notify(String(e)))
    setText('')
    setReplyTo(null)
    setTarget(null)
    onPosted()
  }

  const snippet = (id: string | null) =>
    id ? (byId.get(id)?.content ?? '').split('\n')[0].slice(0, 70) : ''

  return (
    <aside className="panel">
      <div className="panel-head">
        <span>comments</span>
        <button onClick={onClose}>esc</button>
      </div>
      {roots.map((c) => (
        <div key={c.id} className="thread">
          <div className="thread-target">{snippet(c.refers_to)}</div>
          <CommentRow c={c} />
          {repliesOf(c.id).map((r) => (
            <div key={r.id} className="reply">
              <CommentRow c={r} />
            </div>
          ))}
          <button className="chip" onClick={() => setReplyTo(c)}>
            reply
          </button>
        </div>
      ))}
      {roots.length === 0 && !target && (
        <div className="palette-empty">no comments — select text to start a thread</div>
      )}
      {(target || replyTo) && (
        <div className="composer">
          <div className="meta">
            {replyTo ? `replying in thread` : `on: ${snippet(target)}`}
          </div>
          <textarea
            autoFocus
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') post()
            }}
            placeholder="⌘Enter to post"
          />
        </div>
      )}
    </aside>
  )
}

function CommentRow({ c }: { c: Block }) {
  return (
    <div className="comment">
      <span className="comment-text">{c.content}</span>
      <span className="meta">epoch {c.epoch}</span>
    </div>
  )
}
