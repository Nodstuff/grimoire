// The To-dos screen (/todos): every open to-do, grouped Overdue · Today ·
// Upcoming · Someday, with Done folded away. Built in the Home "Today"
// card's language (canvas Main, Mobile-Home, Lapis-System): rounded
// checkboxes, deadline pills (red overdue, yellow today, outline later), the
// ⋯ menu. Storage stays the markdown To-do doc(s): every action is one of
// the daemon's to-do writes (todo.rs), so agents' `- [ ] … · due …` lines
// and the Apple app read the same lists. "Open as markdown" opens the doc.

import { useCallback, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent } from 'react'
import { Button, CountPill, EmptyState, Icon, IconButton, SkeletonLines, useArm } from '../lapis'
import { duePill } from '../homeData'
import { errText, notify } from '../Notice'
import { api } from '../types'
import { addDays, clientClock, dueDate, isNoListYet, moveDueAt, nextMonday, type TodoDay } from '../todo'
import {
  boardLead,
  buildBoard,
  carryOverWrites,
  filterParam,
  GROUPS,
  openCount,
  parsedChip,
  reorderBefore,
  rowKey,
  snoozeDueAt,
  swipeAction,
  visibleOrder,
  writeBody,
  type BoardItem,
  type DueList,
  type GroupId,
  type ListDay,
} from '../todosBoard'
import { workspaceColor, type Workspace } from '../workspaces'
import ChipMenu from './ChipMenu'
import { postTodo, useDuePreview } from './todoHooks'
import './home.css'
import './todos.css'

const FILTER_KEY = 'taisce.todos.filter'
const TICK_MS = 420

function loadFilter(): string {
  try {
    return localStorage.getItem(FILTER_KEY) || 'all'
  } catch {
    return 'all'
  }
}
function storeFilter(v: string) {
  try {
    localStorage.setItem(FILTER_KEY, v)
  } catch {
    /* private window: the filter just isn't remembered */
  }
}

type Pane = 'root' | 'deadline' | 'workspace'

export default function Todos({
  dataVersion,
  workspaces,
  onOpenDoc,
}: {
  dataVersion: number
  workspaces: Workspace[]
  onOpenDoc: (id: string) => void
}) {
  const [filter, setFilterState] = useState<string>(loadFilter)
  const [lists, setLists] = useState<ListDay[] | null>(null)
  const [due, setDue] = useState<DueList | null>(null)
  const [loadErr, setLoadErr] = useState<string | null>(null)
  const [draft, setDraft] = useState('')
  const [note, setNote] = useState('')
  const [noteOn, setNoteOn] = useState(false)
  const [addTo, setAddTo] = useState<string>('unsorted')
  const [adding, setAdding] = useState(false)
  const [sel, setSel] = useState<string | null>(null)
  const [editing, setEditing] = useState<string | null>(null)
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [noteEdit, setNoteEdit] = useState<string | null>(null)
  const [menu, setMenu] = useState<{ key: string; pane: Pane } | null>(null)
  const [ticking, setTicking] = useState<Set<string>>(new Set())
  const [busy, setBusy] = useState<Set<string>>(new Set())
  const [doneOpen, setDoneOpen] = useState(false)
  const [drag, setDrag] = useState<{ key: string; over: string | 'end' | null } | null>(null)
  const [mdMenu, setMdMenu] = useState(false)
  const { armed, arm, disarm } = useArm<string>(6000)
  const addRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  const seq = useRef(0)
  const addId = useId()
  const preview = useDuePreview(draft)
  const today = clientClock().today

  // a filter naming a workspace that is gone falls back to All
  const known = filter === 'all' || filter === 'unsorted' || workspaces.some((w) => w.id === filter)
  const effFilter = known || workspaces.length === 0 ? filter : 'all'
  const setFilter = (v: string) => {
    setFilterState(v)
    storeFilter(v)
  }

  /* ---------- load ---------- */

  const load = useCallback(async () => {
    const mine = ++seq.current
    const c = clientClock()
    const ws = filterParam(effFilter)
    const wsQ = (w: string | null) => (w ? `&workspace=${encodeURIComponent(w)}` : '')
    try {
      // every open deadlined item across days (all lists with no workspace)
      const d = await api<DueList>(`/api/todo/due?today=${c.today}${wsQ(ws)}`)
      // then each list's today: the undated ones and what is done today
      const targets = ws ? [ws] : d.docs && d.docs.length ? [...new Set(d.docs.map((x) => x.workspace_id))] : [null]
      const days = await Promise.all(
        targets.map((w) =>
          api<TodoDay>(`/api/todo?date=${c.today}&today=${c.today}${wsQ(w)}`).catch((e) => {
            // no list there yet, and this caller may not make one
            if (isNoListYet(e) || /read-only/.test(errText(e))) return null
            throw e
          }),
        ),
      )
      if (mine !== seq.current) return
      const seen = new Set<string>()
      const out: ListDay[] = []
      for (const day of days) {
        if (!day || !day.doc_id || seen.has(day.doc_id)) continue
        seen.add(day.doc_id)
        out.push({ day, workspaceId: day.workspace_id ?? null })
        if (day.carried > 0) notify(`${day.carried} to-do${day.carried === 1 ? '' : 's'} carried forward from ${day.prev_date}`, 'ok')
      }
      setDue(d)
      setLists(out)
      setLoadErr(null)
    } catch (e) {
      if (mine !== seq.current) return
      setLoadErr(errText(e))
      notify(`Couldn’t load your to-dos: ${errText(e)} — click to retry`, 'error', { onClick: () => void load() })
    }
  }, [effFilter])
  useEffect(() => {
    void load()
  }, [load, dataVersion])

  const board = useMemo(() => (lists ? buildBoard(lists, due?.items ?? []) : null), [lists, due])
  const order = useMemo(() => (board ? visibleOrder(board, doneOpen) : []), [board, doneOpen])
  const byKey = useMemo(() => new Map(order.concat(board?.done ?? []).map((x) => [x.key, x])), [order, board])
  const multiList = (lists?.length ?? 0) > 1 || (effFilter === 'all' && workspaces.length > 0)

  const wsName = useCallback(
    (id: string | null) => (id ? (workspaces.find((w) => w.id === id)?.display_name ?? workspaces.find((w) => w.id === id)?.name ?? 'Workspace') : 'Unsorted'),
    [workspaces],
  )
  const wsDot = useCallback(
    (id: string | null) => {
      const i = workspaces.findIndex((w) => w.id === id)
      return i < 0 ? 'var(--ink3)' : workspaceColor(workspaces[i], i)
    },
    [workspaces],
  )
  const readOnlyWs = useCallback((id: string | null) => !!id && workspaces.find((w) => w.id === id)?.role === 'viewer', [workspaces])

  /* ---------- writes ---------- */

  const mark = (key: string, on: boolean) =>
    setBusy((s) => {
      const n = new Set(s)
      if (on) n.add(key)
      else n.delete(key)
      return n
    })

  /** One write, then a fresh read. A failure says so with a retry. */
  const write = useCallback(
    async (key: string | null, path: string, body: Record<string, unknown>, ok?: string): Promise<boolean> => {
      if (key) mark(key, true)
      try {
        const d = await postTodo<TodoDay>(path, body)
        if (d?.warning) notify(`${d.warning} — kept as typed; set it from the ⋯ menu`, 'warn')
        if (ok) notify(ok, 'ok')
        await load()
        return true
      } catch (e) {
        notify(`${errText(e)} — click to retry`, 'error', { onClick: () => void write(key, path, body, ok) })
        await load()
        return false
      } finally {
        if (key) mark(key, false)
      }
    },
    [load],
  )

  const toggle = (it: BoardItem) => {
    if (it.done) return void write(it.key, '/api/todo/toggle', writeBody(it, { done: false }))
    // calm: the box fills and the row settles, then it goes to Done
    setTicking((s) => new Set(s).add(it.key))
    setTimeout(async () => {
      await write(it.key, '/api/todo/toggle', writeBody(it, { done: true }))
      setTicking((s) => {
        const n = new Set(s)
        n.delete(it.key)
        return n
      })
    }, TICK_MS)
  }
  const remove = (it: BoardItem) => {
    disarm()
    setMenu(null)
    void write(it.key, '/api/todo/remove', writeBody(it), `Removed “${it.text}”`)
  }
  const snooze = (it: BoardItem) =>
    write(it.key, '/api/todo/deadline', writeBody(it, { due_at: snoozeDueAt() }), `Snoozed to tomorrow 09:00 · “${it.text}”`)
  const setDeadline = (it: BoardItem, dl: { deadline?: string; due_at?: string } | null) => {
    setMenu(null)
    void write(it.key, '/api/todo/deadline', writeBody(it, dl ?? {}))
  }
  const edit = (it: BoardItem, text: string | null) => {
    setEditing(null)
    const t = text?.trim()
    if (t && t !== it.text) void write(it.key, '/api/todo/edit', writeBody(it, { text: t }))
  }
  const saveNote = (it: BoardItem, n: string | null) => {
    setNoteEdit(null)
    if (n !== null && n.trim() !== (it.note ?? '').trim()) void write(it.key, '/api/todo/note', writeBody(it, { note: n }))
  }
  const reorder = (it: BoardItem, before: string | null) => void write(it.key, '/api/todo/reorder', writeBody(it, before ? { before } : {}))

  /** Another workspace's list: add it there (deadline and note carried),
   * then remove it here — added first, so a failure never loses it. */
  const moveToWorkspace = async (it: BoardItem, target: string | null) => {
    setMenu(null)
    mark(it.key, true)
    try {
      const wsBody = target ? { workspace: target } : { workspace: 'unsorted' }
      const day = await postTodo<TodoDay>('/api/todo', { ...wsBody, date: today, text: it.text })
      const added = day.items[day.items.length - 1]
      if (added) {
        for (const [path, body] of carryOverWrites(it, { ...wsBody, date: day.date, item_id: added.id })) await postTodo(path, body)
      }
      await postTodo('/api/todo/remove', writeBody(it))
      notify(`Moved to ${wsName(target)}`, 'ok')
    } catch (e) {
      notify(`${errText(e)} — click to retry`, 'error', { onClick: () => void moveToWorkspace(it, target) })
    } finally {
      mark(it.key, false)
      await load()
    }
  }

  const addTarget = effFilter === 'all' ? addTo : effFilter
  const add = async () => {
    const text = draft.trim()
    if (!text || adding) return
    setAdding(true)
    const n = noteOn ? note.trim() : ''
    const wsBody = addTarget === 'unsorted' && workspaces.length === 0 ? {} : { workspace: addTarget }
    try {
      const day = await postTodo<TodoDay>('/api/todo', { ...wsBody, date: today, text })
      if (day.warning) notify(`${day.warning} — kept as typed; set it from the ⋯ menu`, 'warn')
      const added = day.items[day.items.length - 1]
      if (n && added) await postTodo('/api/todo/note', { ...wsBody, date: day.date, item_id: added.id, note: n })
      setDraft('')
      setNote('')
      setNoteOn(false)
      await load()
    } catch (e) {
      notify(`${errText(e)} — click to retry`, 'error', { onClick: () => void add() })
    } finally {
      setAdding(false)
      addRef.current?.focus()
    }
  }

  /* ---------- keys: J/K X E Enter ⌫ ⌥J/⌥K N ---------- */

  const focusRow = (key: string | null) => {
    setSel(key)
    if (!key) return
    requestAnimationFrame(() => {
      const el = listRef.current?.querySelector<HTMLElement>(`[data-key="${CSS.escape(key)}"]`)
      el?.focus({ preventScroll: true })
      el?.scrollIntoView({ block: 'nearest' })
    })
  }
  const groupFor = (it: BoardItem): BoardItem[] => {
    if (!board) return []
    for (const g of GROUPS) if (board[g.id].some((x) => x.key === it.key)) return board[g.id]
    return []
  }
  const keyRef = useRef<(e: KeyboardEvent) => void>(() => {})
  keyRef.current = (e: KeyboardEvent) => {
    const t = e.target as HTMLElement | null
    if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return
    if (document.querySelector('[role="dialog"], [aria-modal="true"], .palette-backdrop')) return
    // Esc disarms a remove / closes the menu
    if (e.key === 'Escape' && (armed || menu)) {
      disarm()
      setMenu(null)
      return
    }
    const k = rowKey(e)
    if (!k) return
    if (k === 'add') {
      e.preventDefault()
      addRef.current?.focus()
      return
    }
    if (!order.length) return
    const i = order.findIndex((x) => x.key === sel)
    const cur = i >= 0 ? order[i] : null
    // a remove awaiting its confirm: Enter goes ahead
    if (cur && armed === cur.key && k === 'expand') {
      e.preventDefault()
      remove(cur)
      return
    }
    if (k === 'next' || k === 'prev') {
      e.preventDefault()
      const n = cur ? Math.max(0, Math.min(order.length - 1, i + (k === 'next' ? 1 : -1))) : 0
      focusRow(order[n].key)
      return
    }
    if (!cur || readOnlyWs(cur.workspaceId)) return
    e.preventDefault()
    if (k === 'toggle') toggle(cur)
    else if (k === 'edit') setEditing(cur.key)
    else if (k === 'expand') toggleExpand(cur.key)
    else if (k === 'remove') arm(cur.key)
    else if (k === 'moveUp' || k === 'moveDown') {
      const g = groupFor(cur)
      if (g !== board?.someday) return
      const gi = g.findIndex((x) => x.key === cur.key)
      const r = reorderBefore(g, cur.key, gi + (k === 'moveUp' ? -1 : 1))
      if (r) reorder(cur, r.before)
    }
  }
  useEffect(() => {
    const h = (e: KeyboardEvent) => keyRef.current(e)
    window.addEventListener('keydown', h)
    return () => window.removeEventListener('keydown', h)
  }, [])

  // the ⋯ menu closes on a click anywhere else
  useEffect(() => {
    if (!menu && !mdMenu) return
    const onClick = (e: MouseEvent) => {
      const el = e.target as HTMLElement
      if (!el.closest('.hm-menu, .td-more, .td-md')) {
        setMenu(null)
        setMdMenu(false)
      }
    }
    document.addEventListener('mousedown', onClick)
    return () => document.removeEventListener('mousedown', onClick)
  }, [menu, mdMenu])

  const toggleExpand = (key: string) =>
    setExpanded((s) => {
      const n = new Set(s)
      if (n.has(key)) n.delete(key)
      else n.add(key)
      return n
    })

  /* ---------- render ---------- */

  const chip = parsedChip(preview, today)
  const docs = lists?.map((l) => ({ id: l.day.doc_id, ws: l.workspaceId })) ?? []
  const filterOptions = [
    { id: 'all', label: 'All' },
    { id: 'unsorted', label: 'Unsorted', dot: 'var(--ink3)' },
    ...workspaces.map((w, i) => ({ id: w.id, label: w.display_name || w.name, dot: workspaceColor(w, i) })),
  ]
  const addOptions = filterOptions.filter((o) => o.id !== 'all' && !readOnlyWs(o.id === 'unsorted' ? null : o.id))
  const addReadOnly = readOnlyWs(addTarget === 'unsorted' ? null : addTarget)
  const now = new Date()
  const dateLine = now.toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long' })

  const openMarkdown = () => {
    if (docs.length === 1) onOpenDoc(docs[0].id)
    else if (docs.length > 1) setMdMenu((o) => !o)
  }

  const row = (it: BoardItem, group: GroupId) => (
    <TodoRow
      key={it.key}
      it={it}
      today={today}
      group={group}
      selected={sel === it.key}
      ticking={ticking.has(it.key)}
      busy={busy.has(it.key)}
      readOnly={readOnlyWs(it.workspaceId)}
      editing={editing === it.key}
      expanded={expanded.has(it.key)}
      noteEditing={noteEdit === it.key}
      menu={menu?.key === it.key ? menu.pane : null}
      armed={armed === it.key}
      wsLabel={multiList ? { name: wsName(it.workspaceId), dot: wsDot(it.workspaceId) } : null}
      moveTargets={addOptions.filter((o) => o.id !== (it.workspaceId ?? 'unsorted'))}
      draggable={group === 'someday' && !readOnlyWs(it.workspaceId)}
      dropBefore={drag?.over === it.key && drag.key !== it.key}
      onSelect={() => setSel(it.key)}
      onToggle={() => toggle(it)}
      onEdit={() => setEditing(it.key)}
      onEditDone={(t) => edit(it, t)}
      onExpand={() => toggleExpand(it.key)}
      onMenu={(p) => setMenu(p ? { key: it.key, pane: p } : null)}
      onDeadline={(dl) => setDeadline(it, dl)}
      onSnooze={() => {
        setMenu(null)
        void snooze(it)
      }}
      onNoteEdit={() => {
        setMenu(null)
        setExpanded((s) => new Set(s).add(it.key))
        setNoteEdit(it.key)
      }}
      onNoteDone={(n) => saveNote(it, n)}
      onMoveWorkspace={(w) => void moveToWorkspace(it, w === 'unsorted' ? null : w)}
      onArm={() => arm(it.key)}
      onDisarm={disarm}
      onRemove={() => remove(it)}
      onDragStart={() => setDrag({ key: it.key, over: null })}
      onDragOver={() => drag && drag.over !== it.key && setDrag({ ...drag, over: it.key })}
      onDragEnd={() => setDrag(null)}
      onDrop={() => {
        if (!drag || !board) return
        const g = board.someday
        const dragged = g.find((x) => x.key === drag.key)
        const to = g.findIndex((x) => x.key === it.key)
        const from = g.findIndex((x) => x.key === drag.key)
        setDrag(null)
        if (!dragged || to < 0) return
        const r = reorderBefore(g, drag.key, from < to ? to - 1 : to)
        if (r) reorder(dragged, r.before)
      }}
      onSwipe={(a) => (a === 'done' ? toggle(it) : void snooze(it))}
    />
  )

  return (
    <div className="lp-page td-page">
      <header className="lp-page-head">
        <div className="lp-page-head-text">
          <div className="lp-page-kicker muted">{dateLine}</div>
          <h1 className="lp-page-title">To-dos</h1>
          <p className="lp-page-lead">{board ? boardLead(board) : ' '}</p>
        </div>
        <div className="lp-page-actions td-actions">
          {workspaces.length > 0 && <ChipMenu label="Workspace" value={effFilter} options={filterOptions} onChange={setFilter} />}
          {docs.length > 0 && (
            <div className="td-md">
              <button type="button" className="hm-link" onClick={openMarkdown} aria-haspopup={docs.length > 1 ? 'menu' : undefined} aria-expanded={docs.length > 1 ? mdMenu : undefined}>
                <Icon name="doc" size={15} />
                Open as markdown
              </button>
              {mdMenu && (
                <div className="hm-menu td-md-menu" role="menu">
                  {docs.map((d) => (
                    <button key={d.id} type="button" role="menuitem" onClick={() => onOpenDoc(d.id)}>
                      <span className="td-ws">
                        <span className="lp-dot" style={{ background: wsDot(d.ws) }} aria-hidden />
                        To-do · {wsName(d.ws)}
                      </span>
                    </button>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      </header>

      <form
        className="lp-card td-add"
        onSubmit={(e) => {
          e.preventDefault()
          void add()
        }}
      >
        <div className="td-add-row">
          <span className="td-add-plus" aria-hidden>
            <Icon name="plus" size={18} />
          </span>
          <input
            id={addId}
            ref={addRef}
            aria-label="Add a to-do"
            className="td-add-input"
            placeholder="Add a to-do…"
            value={draft}
            disabled={adding || addReadOnly}
            autoComplete="off"
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                e.stopPropagation()
                setDraft('')
                e.currentTarget.blur()
              }
            }}
          />
          {chip && (
            <span className={`td-chip${chip.warn ? ' warn' : ''}`} aria-live="polite" title={chip.warn ? undefined : 'The deadline it will get'}>
              <Icon name={chip.warn ? 'warning' : 'clock'} size={13} />
              {chip.text}
            </span>
          )}
          <Button type="submit" variant="primary" size="sm" disabled={!draft.trim() || adding || addReadOnly} className="td-add-btn">
            Add
          </Button>
        </div>
        {noteOn && (
          <textarea
            className="hm-field td-add-note"
            aria-label="Note for the new to-do"
            placeholder="A note under it (optional)"
            rows={2}
            value={note}
            onChange={(e) => setNote(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                e.preventDefault()
                void add()
              }
            }}
          />
        )}
        <div className="td-add-foot">
          <button type="button" className="hm-link quiet" aria-pressed={noteOn} onClick={() => setNoteOn((o) => !o)}>
            <Icon name="comment" size={14} />
            {noteOn ? 'No note' : 'Add a note'}
          </button>
          {effFilter === 'all' && workspaces.length > 0 && (
            <ChipMenu label="Add to" value={addTo} options={addOptions} onChange={setAddTo} />
          )}
          <span className="td-add-hint">
            <kbd className="lp-kbd">↵</kbd> adds · “due fri 3pm” sets a deadline
          </span>
        </div>
      </form>

      <div className="lp-card lp-card-flush td-list" ref={listRef} aria-busy={!board}>
        {!board && !loadErr && (
          <div className="td-loading">
            <SkeletonLines lines={5} />
          </div>
        )}
        {!board && loadErr && (
          <EmptyState
            compact
            icon="error"
            title="Couldn’t load your to-dos"
            action={
              <Button icon="history" onClick={() => void load()}>
                Retry
              </Button>
            }
          >
            {loadErr}
          </EmptyState>
        )}
        {board && openCount(board) === 0 && board.done.length === 0 && (
          <EmptyState compact icon="todo" title="Nothing to do">
            Add one above. Agents can add to-dos too: they append to the To-do doc and show up here.
          </EmptyState>
        )}
        {board && openCount(board) === 0 && board.done.length > 0 && (
          <div className="td-allclear">
            <Icon name="check" size={16} /> All done for today.
          </div>
        )}
        {board &&
          GROUPS.filter((g) => g.id !== 'done' && board[g.id].length > 0).map((g) => (
            <section key={g.id} className={`td-group td-group-${g.id}`} aria-labelledby={`td-h-${g.id}`}>
              <h2 className="td-group-head" id={`td-h-${g.id}`}>
                {g.id === 'overdue' && <Icon name="clock" size={15} />}
                {g.label}
                <CountPill n={board[g.id].length} />
                {g.id === 'someday' && <span className="td-group-sub">no date</span>}
              </h2>
              <ul
                className={`td-rows${drag && g.id === 'someday' && drag.over === 'end' ? ' drop-end' : ''}`}
                onDragOver={(e) => {
                  if (g.id !== 'someday' || !drag) return
                  e.preventDefault()
                  // below the last row: drop at the end
                  if (e.target === e.currentTarget && drag.over !== 'end') setDrag({ ...drag, over: 'end' })
                }}
                onDrop={(e) => {
                  if (g.id !== 'someday' || !drag || drag.over !== 'end') return
                  e.preventDefault()
                  const dragged = board.someday.find((x) => x.key === drag.key)
                  setDrag(null)
                  const r = dragged && reorderBefore(board.someday, dragged.key, board.someday.length - 1)
                  if (dragged && r) reorder(dragged, r.before)
                }}
              >
                {board[g.id].map((it) => row(it, g.id))}
              </ul>
            </section>
          ))}
        {board && board.done.length > 0 && (
          <section className="td-group td-group-done" aria-labelledby="td-h-done">
            <h2 className="td-group-head" id="td-h-done">
              <button type="button" className="td-done-toggle" aria-expanded={doneOpen} onClick={() => setDoneOpen((o) => !o)}>
                <Icon name={doneOpen ? 'chevronDown' : 'chevronRight'} size={15} />
                Done
                <CountPill n={board.done.length} />
              </button>
            </h2>
            {doneOpen && <ul className="td-rows">{board.done.map((it) => row(it, 'done'))}</ul>}
          </section>
        )}
        {board && order.length > 0 && (
          <div className="td-keys" aria-hidden>
            <kbd className="lp-kbd">J</kbd>
            <kbd className="lp-kbd">K</kbd> move · <kbd className="lp-kbd">X</kbd> done · <kbd className="lp-kbd">E</kbd> edit · <kbd className="lp-kbd">↵</kbd> note ·{' '}
            <kbd className="lp-kbd">⌫</kbd> remove · <kbd className="lp-kbd">N</kbd> new
          </div>
        )}
      </div>

      <button
        type="button"
        className="td-fab"
        aria-label="Add a to-do"
        onClick={() => {
          addRef.current?.scrollIntoView({ block: 'center', behavior: 'smooth' })
          addRef.current?.focus()
        }}
      >
        <Icon name="plus" size={24} />
      </button>
    </div>
  )
}

/* ---------- a row ---------- */

function TodoRow({
  it,
  today,
  group,
  selected,
  ticking,
  busy,
  readOnly,
  editing,
  expanded,
  noteEditing,
  menu,
  armed,
  wsLabel,
  moveTargets,
  draggable,
  dropBefore,
  onSelect,
  onToggle,
  onEdit,
  onEditDone,
  onExpand,
  onMenu,
  onDeadline,
  onSnooze,
  onNoteEdit,
  onNoteDone,
  onMoveWorkspace,
  onArm,
  onDisarm,
  onRemove,
  onDragStart,
  onDragOver,
  onDragEnd,
  onDrop,
  onSwipe,
}: {
  it: BoardItem
  today: string
  group: GroupId
  selected: boolean
  ticking: boolean
  busy: boolean
  readOnly: boolean
  editing: boolean
  expanded: boolean
  noteEditing: boolean
  menu: Pane | null
  armed: boolean
  wsLabel: { name: string; dot: string } | null
  moveTargets: { id: string; label: string; dot?: string }[]
  draggable: boolean
  dropBefore: boolean
  onSelect: () => void
  onToggle: () => void
  onEdit: () => void
  onEditDone: (text: string | null) => void
  onExpand: () => void
  onMenu: (p: Pane | null) => void
  onDeadline: (dl: { deadline?: string; due_at?: string } | null) => void
  onSnooze: () => void
  onNoteEdit: () => void
  onNoteDone: (note: string | null) => void
  onMoveWorkspace: (ws: string) => void
  onArm: () => void
  onDisarm: () => void
  onRemove: () => void
  onDragStart: () => void
  onDragOver: () => void
  onDragEnd: () => void
  onDrop: () => void
  onSwipe: (a: 'done' | 'snooze') => void
}) {
  const [text, setText] = useState(it.text)
  const [note, setNote] = useState(it.note ?? '')
  const [pickDay, setPickDay] = useState('')
  const [pickTime, setPickTime] = useState('')
  const [dx, setDx] = useState(0)
  const swipe = useRef<{ x: number; y: number; on: boolean; id: number } | null>(null)
  const editRef = useRef<HTMLInputElement>(null)
  const noteRef = useRef<HTMLTextAreaElement>(null)
  const editPreview = useDuePreview(editing ? text : '')
  useEffect(() => {
    if (editing) {
      setText(it.text)
      requestAnimationFrame(() => {
        editRef.current?.focus()
        editRef.current?.select()
      })
    }
  }, [editing, it.text])
  useEffect(() => {
    if (noteEditing) {
      setNote(it.note ?? '')
      requestAnimationFrame(() => noteRef.current?.focus())
    }
  }, [noteEditing, it.note])
  useEffect(() => {
    if (menu === 'deadline') {
      const d = dueDate(it)
      setPickDay(d ?? '')
      setPickTime(it.due_at ? new Date(it.due_at).toTimeString().slice(0, 5) : '')
    }
  }, [menu, it])

  const done = it.done || ticking
  const pill = duePill({ ...it, done }, today)
  const due = dueDate(it)
  const chip = editing ? parsedChip(editPreview, today) : null
  const canWrite = !readOnly && !busy

  // phone: swipe right completes, left snoozes (touch only)
  const onPointerDown = (e: ReactPointerEvent) => {
    if (e.pointerType !== 'touch' || !canWrite || editing || it.done) return
    swipe.current = { x: e.clientX, y: e.clientY, on: false, id: e.pointerId }
  }
  const onPointerMove = (e: ReactPointerEvent) => {
    const s = swipe.current
    if (!s) return
    const ddx = e.clientX - s.x
    const ddy = e.clientY - s.y
    if (!s.on) {
      if (Math.abs(ddy) > 10 && Math.abs(ddy) > Math.abs(ddx)) {
        swipe.current = null
        return
      }
      if (Math.abs(ddx) < 10) return
      s.on = true
      try {
        ;(e.currentTarget as HTMLElement).setPointerCapture(s.id)
      } catch {
        /* a pointer the browser no longer tracks */
      }
    }
    setDx(Math.max(-140, Math.min(140, ddx)))
  }
  const onPointerUp = () => {
    const s = swipe.current
    swipe.current = null
    if (!s?.on) return
    const a = swipeAction(dx)
    setDx(0)
    if (a) onSwipe(a)
  }

  const deadlineFromPick = (): { deadline?: string; due_at?: string } | null => {
    if (!pickDay) return null
    if (pickTime) {
      const [y, m, d] = pickDay.split('-').map(Number)
      const [h, mi] = pickTime.split(':').map(Number)
      return { due_at: new Date(y, m - 1, d, h, mi).toISOString() }
    }
    return { deadline: pickDay }
  }

  return (
    <li
      className={[
        'td-row',
        done ? 'done' : '',
        ticking ? 'ticking' : '',
        selected ? 'sel' : '',
        menu ? 'menu-open' : '',
        busy ? 'busy' : '',
        dropBefore ? 'drop-before' : '',
        dx > 0 ? 'swipe-r' : dx < 0 ? 'swipe-l' : '',
      ]
        .filter(Boolean)
        .join(' ')}
      data-key={it.key}
      tabIndex={selected ? 0 : -1}
      aria-label={`${it.text}${due ? `, ${pill?.text ?? ''}` : ''}${it.done ? ', done' : ''}`}
      onFocus={(e) => e.target === e.currentTarget && onSelect()}
      draggable={draggable && !editing && !noteEditing}
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = 'move'
        e.dataTransfer.setData('text/plain', it.text)
        onDragStart()
      }}
      onDragOver={(e) => {
        if (!draggable) return
        e.preventDefault()
        e.stopPropagation()
        onDragOver()
      }}
      onDragEnd={onDragEnd}
      onDrop={(e) => {
        if (!draggable) return
        e.preventDefault()
        e.stopPropagation()
        onDrop()
      }}
    >
      {dx !== 0 && (
        <div className="td-swipe-bg" aria-hidden>
          <span className="td-swipe-done">
            <Icon name="check" size={18} /> Done
          </span>
          <span className="td-swipe-snooze">
            Tomorrow 09:00 <Icon name="clock" size={18} />
          </span>
        </div>
      )}
      <div
        className="td-main"
        style={dx ? { transform: `translateX(${dx}px)` } : undefined}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={() => {
          swipe.current = null
          setDx(0)
        }}
        onClick={(e) => {
          onSelect()
          // a click on the row's empty space shows the note
          if (e.target === e.currentTarget && it.note) onExpand()
        }}
      >
        {draggable && <span className="td-grip" aria-hidden title="Drag to reorder" />}
        <button
          type="button"
          className={`hm-check td-check ${done ? 'done' : 'open'}`}
          role="checkbox"
          aria-checked={done}
          aria-label={`${it.text}: ${it.done ? 'mark not done' : 'mark done'}`}
          disabled={!canWrite}
          onClick={(e) => {
            e.stopPropagation()
            onToggle()
          }}
        >
          {done && <Icon name="check" size={13} strokeWidth={2.5} />}
        </button>
        {editing ? (
          <span className="td-edit-wrap">
            <input
              ref={editRef}
              className="td-edit"
              aria-label="Edit the to-do"
              value={text}
              onChange={(e) => setText(e.target.value)}
              onBlur={() => onEditDone(text)}
              onKeyDown={(e: ReactKeyboardEvent<HTMLInputElement>) => {
                if (e.key === 'Enter') {
                  e.preventDefault()
                  onEditDone(text)
                } else if (e.key === 'Escape') {
                  e.stopPropagation()
                  onEditDone(null)
                }
              }}
            />
            {chip && <span className={`td-chip${chip.warn ? ' warn' : ''}`}>{chip.text}</span>}
          </span>
        ) : (
          <span className="td-text" onClick={() => canWrite && !it.done && onEdit()} title={canWrite && !it.done ? 'Click to edit (E)' : undefined}>
            {it.text}
            {it.carried_from && <span className="td-sub">from {it.carried_from === addDays(today, -1) ? 'yesterday' : it.carried_from}</span>}
          </span>
        )}
        {wsLabel && (
          <span className="td-ws td-ws-tag">
            <span className="lp-dot" style={{ background: wsLabel.dot }} aria-hidden />
            {wsLabel.name}
          </span>
        )}
        {it.note && !editing && (
          <IconButton icon="comment" size="sm" className="td-note-btn" active={expanded} label={expanded ? 'Hide note' : 'Show note'} onClick={onExpand} />
        )}
        {pill && (
          <span className={`hm-pill ${pill.tone}`} title={`Deadline ${it.due_at ? new Date(it.due_at).toLocaleString() : due}`}>
            {pill.text}
          </span>
        )}
        {!readOnly && group !== 'done' && (
          <IconButton icon="more" size="sm" className="td-more" label="Deadline, note, workspace, remove" aria-haspopup="menu" aria-expanded={!!menu} onClick={() => onMenu(menu ? null : 'root')} />
        )}
        {!readOnly && group === 'done' && (
          <IconButton icon="close" size="sm" className="td-more" label="Remove" onClick={onArm} />
        )}
      </div>
      {armed && (
        <div className="td-confirm" role="alert">
          <span>Remove “{it.text}”?</span>
          <Button size="sm" variant="danger" onClick={onRemove}>
            Remove
          </Button>
          <Button size="sm" variant="ghost" onClick={onDisarm}>
            Cancel
          </Button>
        </div>
      )}
      {it.note && expanded && !noteEditing && (
        <div className="td-note" onClick={readOnly ? undefined : onNoteEdit} title={readOnly ? undefined : 'Click to edit the note'}>
          {it.note}
        </div>
      )}
      {it.note && !expanded && !noteEditing && (
        <button type="button" className="td-note preview" onClick={onExpand}>
          {it.note.split('\n')[0]}
        </button>
      )}
      {noteEditing && (
        <textarea
          ref={noteRef}
          className="hm-field td-note-edit"
          aria-label="Note"
          rows={Math.min(6, Math.max(2, note.split('\n').length + 1))}
          placeholder="A note under the item. ⌘↵ saves, Esc cancels"
          value={note}
          onChange={(e) => setNote(e.target.value)}
          onBlur={() => onNoteDone(note)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
              e.preventDefault()
              onNoteDone(note)
            } else if (e.key === 'Escape') {
              e.stopPropagation()
              onNoteDone(null)
            }
          }}
        />
      )}
      {menu && (
        <div className="hm-menu td-menu" role="menu" onKeyDown={(e) => e.key === 'Escape' && (e.stopPropagation(), onMenu(null))}>
          {menu === 'root' && (
            <>
              <button type="button" role="menuitem" onClick={() => onMenu('deadline')}>
                {due ? 'Change deadline…' : 'Set a deadline…'}
                <Icon name="chevronRight" size={15} />
              </button>
              <button type="button" role="menuitem" onClick={onSnooze}>
                Snooze to tomorrow 09:00
              </button>
              <button type="button" role="menuitem" onClick={onNoteEdit}>
                {it.note ? 'Edit note' : 'Add a note'}
              </button>
              {moveTargets.length > 0 && (
                <button type="button" role="menuitem" onClick={() => onMenu('workspace')}>
                  Move to workspace…
                  <Icon name="chevronRight" size={15} />
                </button>
              )}
              <button
                type="button"
                role="menuitem"
                className="danger"
                onClick={() => {
                  onMenu(null)
                  onArm()
                }}
              >
                Remove…
              </button>
            </>
          )}
          {menu === 'deadline' && (
            <>
              <button type="button" role="menuitem" onClick={() => onDeadline(it.due_at ? { due_at: moveDueAt(it.due_at, today) } : { deadline: today })}>
                Today
              </button>
              <button type="button" role="menuitem" onClick={() => onDeadline(it.due_at ? { due_at: moveDueAt(it.due_at, addDays(today, 1)) } : { deadline: addDays(today, 1) })}>
                Tomorrow
              </button>
              <button type="button" role="menuitem" onClick={() => onDeadline(it.due_at ? { due_at: moveDueAt(it.due_at, nextMonday(today)) } : { deadline: nextMonday(today) })}>
                Next Monday
              </button>
              <div className="hm-menu-pick">
                Pick a day, and a time if it has one
                <span>
                  <input type="date" className="hm-field" aria-label="Day" value={pickDay} onChange={(e) => setPickDay(e.target.value)} />
                  <input type="time" className="hm-field td-time" aria-label="Time" value={pickTime} onChange={(e) => setPickTime(e.target.value)} />
                </span>
                <Button size="sm" variant="primary" disabled={!pickDay} onClick={() => onDeadline(deadlineFromPick())}>
                  Set deadline
                </Button>
              </div>
              {due && (
                <button type="button" role="menuitem" onClick={() => onDeadline(null)}>
                  Clear the deadline
                </button>
              )}
            </>
          )}
          {menu === 'workspace' &&
            moveTargets.map((w) => (
              <button key={w.id} type="button" role="menuitem" onClick={() => onMoveWorkspace(w.id)}>
                <span className="td-ws">
                  <span className="lp-dot" style={{ background: w.dot ?? 'var(--ink3)' }} aria-hidden />
                  {w.label}
                </span>
              </button>
            ))}
        </div>
      )}
    </li>
  )
}
