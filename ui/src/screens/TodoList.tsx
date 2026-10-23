// Home's Today card body (canvas Main): today's items out of the daemon's
// per-day list (`/api/todo`, one `## YYYY-MM-DD` heading per day in the To-do
// doc), grouped Overdue / Today / Done, each row a rounded checkbox, the
// text (click to edit), a hover ⋯ menu (move / deadline / note) and × and
// the deadline pill on the right; the "Add a to-do" field at the foot (Enter
// adds, Esc clears, `due …` previewed by the daemon), and a read-only look
// at the previous day. Every write is a POST that
// answers with the fresh day, so the list never guesses.

import { useCallback, useEffect, useId, useRef, useState } from 'react'
import { Button, Icon, IconButton, SkeletonLines } from '../lapis'
import { duePill, groupTodos } from '../homeData'
import { errText, notify } from '../Notice'
import { api } from '../types'
import { addDays, clientClock, dueDate, emptyDay, fmtDay, isNoListYet, moveDueAt, nextMonday, previewLabel, type TodoDay, type TodoItem } from '../todo'
import { postTodo, useDuePreview } from './todoHooks'

type Menu = { id: string; pane: 'root' | 'move' | 'deadline' } | null

export default function Todo({
  date,
  dataVersion,
  readOnly,
  onLoaded,
  onOpenDoc,
}: {
  /** the day shown; writes go to it */
  date: string
  dataVersion: number
  /** yesterday's view: no editing */
  readOnly?: boolean
  /** the parent's header summary needs the open count */
  onLoaded?: (day: TodoDay | null) => void
  onOpenDoc: (id: string) => void
}) {
  const [day, setDay] = useState<TodoDay | null>(null)
  const [missing, setMissing] = useState(false)
  const [busy, setBusy] = useState(false)
  const [draft, setDraft] = useState('')
  const [editing, setEditing] = useState<string | null>(null)
  const [noteOpen, setNoteOpen] = useState<Set<string>>(new Set())
  const [noteEdit, setNoteEdit] = useState<string | null>(null)
  const [menu, setMenu] = useState<Menu>(null)
  const addId = useId()

  const apply = useCallback(
    (d: TodoDay | null) => {
      setDay(d)
      onLoaded?.(d)
    },
    [onLoaded],
  )

  const load = useCallback(() => {
    api<TodoDay>(`/api/todo?date=${encodeURIComponent(date)}&today=${clientClock().today}`)
      .then((d) => {
        setMissing(false)
        apply(d)
        if (d.carried > 0) notify(`${d.carried} to-do${d.carried === 1 ? '' : 's'} carried forward from ${d.prev_date}`, 'ok')
      })
      .catch((e) => {
        // a browser session's GET never creates the list (SERVER mode): an
        // empty day, and the first add makes it
        if (isNoListYet(e)) {
          setMissing(false)
          apply(emptyDay(date, clientClock().today))
          return
        }
        // an older daemon without the route: the section says so quietly
        setMissing(true)
        apply(null)
        if (!/404|not found/i.test(errText(e))) notify(errText(e))
      })
  }, [date, apply])
  useEffect(load, [load, dataVersion])

  const post = async (path: string, body: Record<string, unknown>) => {
    setBusy(true)
    try {
      const d = await postTodo<TodoDay>(path, { date, ...body })
      apply(d)
      if (d.warning) notify(`${d.warning} — kept as typed; set it from the ⋯ menu`, 'warn')
      return true
    } catch (e) {
      notify(errText(e))
      load()
      return false
    } finally {
      setBusy(false)
    }
  }

  const add = async () => {
    const text = draft.trim()
    if (!text) return
    setDraft('')
    if (!(await post('/api/todo', { text }))) setDraft(text)
  }

  // the live hint: a draft ending in `due …` / `by …` is shown to the daemon
  // (debounced) and its reading echoed beside the row; the rules live there
  const preview = useDuePreview(draft)

  // the ⋯ menu closes on Esc and on a click anywhere else
  useEffect(() => {
    if (!menu) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        setMenu(null)
      }
    }
    const onClick = (e: MouseEvent) => {
      if (!(e.target as HTMLElement).closest('.hm-menu, .hm-todo-more')) setMenu(null)
    }
    document.addEventListener('keydown', onKey, true)
    document.addEventListener('mousedown', onClick)
    return () => {
      document.removeEventListener('keydown', onKey, true)
      document.removeEventListener('mousedown', onClick)
    }
  }, [menu])

  if (missing) return <div className="hm-empty">This Taisce is older than the to-do list. Update it to plan your day here.</div>
  if (!day) return <div className="hm-todo-loading"><SkeletonLines lines={3} /></div>

  const today = day.today
  const items = day.items
  const groups = groupTodos(items)
  const toggleNote = (id: string) =>
    setNoteOpen((s) => {
      const n = new Set(s)
      if (n.has(id)) n.delete(id)
      else n.add(id)
      return n
    })

  const row = (it: TodoItem) => (
    <TodoRow
      key={it.id}
      it={it}
      today={today}
      readOnly={!!readOnly}
      busy={busy}
      editing={editing === it.id}
      noteOpen={noteOpen.has(it.id)}
      noteEditing={noteEdit === it.id}
      menu={menu?.id === it.id ? menu.pane : null}
      onToggle={() => post('/api/todo/toggle', { item_id: it.id, done: !it.done })}
      onEditStart={() => !readOnly && setEditing(it.id)}
      onEditDone={(text) => {
        setEditing(null)
        if (text !== null && text.trim() && text.trim() !== it.text) post('/api/todo/edit', { item_id: it.id, text })
      }}
      onRemove={() => post('/api/todo/remove', { item_id: it.id })}
      onMenu={(pane) => setMenu(pane ? { id: it.id, pane } : null)}
      onMove={(to) => {
        setMenu(null)
        post('/api/todo/move', { item_id: it.id, to_date: to }).then((ok) => ok && notify(`Moved to ${fmtDay(to, today)}`, 'ok'))
      }}
      onDeadline={(dl) => {
        setMenu(null)
        // a timed deadline keeps its local time on the new date
        if (dl && it.due_at) post('/api/todo/deadline', { item_id: it.id, due_at: moveDueAt(it.due_at, dl) })
        else post('/api/todo/deadline', { item_id: it.id, deadline: dl })
      }}
      onNoteToggle={() => toggleNote(it.id)}
      onNoteEdit={() => {
        setMenu(null)
        setNoteOpen((s) => new Set(s).add(it.id))
        setNoteEdit(it.id)
      }}
      onNoteDone={(note) => {
        setNoteEdit(null)
        if (note !== null && note.trim() !== (it.note ?? '').trim()) post('/api/todo/note', { item_id: it.id, note })
      }}
    />
  )
  // group labels only when there is more than one kind of row
  const kinds = [groups.overdue, groups.today, groups.done].filter((g) => g.length > 0).length
  const labels = kinds > 1 || groups.overdue.length > 0

  return (
    <div className={`hm-todo${readOnly ? ' readonly' : ''}`}>
      {items.length === 0 && (
        <div className="hm-empty">{readOnly ? `Nothing was on ${fmtDay(date, today)}.` : 'Nothing planned yet. Add the first to-do below.'}</div>
      )}
      {groups.overdue.length > 0 && (
        <>
          <div className="hm-todo-label late">
            <Icon name="clock" size={15} />
            Overdue
          </div>
          <ul className="hm-todo-list">{groups.overdue.map(row)}</ul>
        </>
      )}
      {groups.today.length > 0 && (
        <>
          {labels && <div className="hm-todo-label">{readOnly ? fmtDay(date, today).replace(/^./, (c) => c.toUpperCase()) : 'Today'}</div>}
          <ul className="hm-todo-list">{groups.today.map(row)}</ul>
        </>
      )}
      {groups.done.length > 0 && (
        <>
          {labels && <div className="hm-todo-label">Done</div>}
          <ul className="hm-todo-list">{groups.done.map(row)}</ul>
        </>
      )}
      {!readOnly && (
        <div className="hm-todo-add">
          <div className="hm-todo-add-head">
            <label htmlFor={addId}>Add a to-do</label>
            {preview && previewLabel(preview, today) ? (
              <span className={`hm-todo-hint${preview.warning ? ' warn' : ''}`} title={preview.deadline ?? undefined} aria-live="polite">
                {previewLabel(preview, today)}
              </span>
            ) : (
              <span className="hm-todo-hint">“due fri 3pm” sets a deadline</span>
            )}
          </div>
          <input
            id={addId}
            className="hm-field"
            placeholder="Pick up the Kindle cable due fri 3pm"
            value={draft}
            disabled={busy}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault()
                add()
              } else if (e.key === 'Escape') {
                e.stopPropagation()
                setDraft('')
                ;(e.target as HTMLInputElement).blur()
              }
            }}
          />
          {day.doc_id && (
            <button type="button" className="hm-link hm-todo-doc" onClick={() => onOpenDoc(day.doc_id)}>
              Open the To-do doc
            </button>
          )}
        </div>
      )}
    </div>
  )
}

function TodoRow({
  it,
  today,
  readOnly,
  busy,
  editing,
  noteOpen,
  noteEditing,
  menu,
  onToggle,
  onEditStart,
  onEditDone,
  onRemove,
  onMenu,
  onMove,
  onDeadline,
  onNoteToggle,
  onNoteEdit,
  onNoteDone,
}: {
  it: TodoItem
  today: string
  readOnly: boolean
  busy: boolean
  editing: boolean
  noteOpen: boolean
  noteEditing: boolean
  menu: 'root' | 'move' | 'deadline' | null
  onToggle: () => void
  onEditStart: () => void
  onEditDone: (text: string | null) => void
  onRemove: () => void
  onMenu: (pane: 'root' | 'move' | 'deadline' | null) => void
  onMove: (to: string) => void
  onDeadline: (deadline: string | null) => void
  onNoteToggle: () => void
  onNoteEdit: () => void
  onNoteDone: (note: string | null) => void
}) {
  const [text, setText] = useState(it.text)
  const [note, setNote] = useState(it.note ?? '')
  const [pick, setPick] = useState('')
  const editRef = useRef<HTMLInputElement>(null)
  const noteRef = useRef<HTMLTextAreaElement>(null)
  useEffect(() => {
    if (editing) {
      setText(it.text)
      editRef.current?.focus()
      editRef.current?.select()
    }
  }, [editing, it.text])
  useEffect(() => {
    if (noteEditing) {
      setNote(it.note ?? '')
      noteRef.current?.focus()
    }
  }, [noteEditing, it.note])

  const pill = duePill(it, today)
  const due = dueDate(it)
  const state = it.done ? 'done' : it.carried ? 'carried' : 'open'
  const notePreview = it.note?.split('\n')[0] ?? ''

  return (
    <li className={`hm-todo-item ${state}${menu ? ' menu-open' : ''}`}>
      <div className="hm-todo-main">
        <button
          type="button"
          className={`hm-check ${state}`}
          role="checkbox"
          aria-checked={it.done}
          aria-label={it.text}
          disabled={busy || readOnly || !!it.carried}
          title={it.carried ? 'Moved to a later day' : it.done ? 'Mark not done' : 'Mark done'}
          onClick={onToggle}
        >
          {it.done && <Icon name="check" size={13} strokeWidth={2.5} />}
          {it.carried && <Icon name="chevronRight" size={13} strokeWidth={2.2} />}
        </button>
        {editing ? (
          <input
            ref={editRef}
            className="hm-todo-edit"
            aria-label="Edit the to-do"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onBlur={() => onEditDone(text)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault()
                onEditDone(text)
              } else if (e.key === 'Escape') {
                e.stopPropagation()
                onEditDone(null)
              }
            }}
          />
        ) : (
          <span className="hm-todo-text" onClick={onEditStart} title={readOnly ? undefined : 'Click to edit'}>
            {it.text}
            {it.carried_from && <span className="hm-todo-carried">from {fmtDay(it.carried_from, today)}</span>}
          </span>
        )}
        {it.note && !editing && (
          <IconButton
            icon="comment"
            size="sm"
            className="hm-todo-note-btn"
            active={noteOpen}
            label={noteOpen ? 'Hide note' : 'Show note'}
            onClick={onNoteToggle}
          />
        )}
        {!readOnly && (
          <span className="hm-todo-actions">
            <IconButton icon="more" size="sm" className="hm-todo-more" label="Move, deadline, note" onClick={() => onMenu(menu ? null : 'root')} />
            <IconButton icon="close" size="sm" className="hm-todo-remove" label="Remove" disabled={busy} onClick={onRemove} />
          </span>
        )}
        {pill && (
          <span
            className={`hm-pill ${pill.tone}`}
            title={`Deadline ${it.due_at ? new Date(it.due_at).toLocaleString() : due}`}
          >
            {pill.text}
          </span>
        )}
      </div>
      {it.note && noteOpen && !noteEditing && (
        <div className="hm-todo-note" onClick={readOnly ? undefined : onNoteEdit} title={readOnly ? undefined : 'Click to edit the note'}>
          {it.note}
        </div>
      )}
      {it.note && !noteOpen && !noteEditing && (
        <div className="hm-todo-note preview" onClick={onNoteToggle}>
          {notePreview}
        </div>
      )}
      {noteEditing && (
        <textarea
          ref={noteRef}
          className="hm-field hm-todo-note-edit"
          aria-label="Note"
          rows={Math.min(6, Math.max(2, note.split('\n').length + 1))}
          placeholder="A note under the item. ⌘Enter saves, Esc cancels"
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
        <div className="hm-menu" role="menu">
          {menu === 'root' && (
            <>
              <button type="button" role="menuitem" onClick={() => onMenu('move')}>
                Move to…
                <Icon name="chevronRight" size={15} />
              </button>
              <button type="button" role="menuitem" onClick={() => onMenu('deadline')}>
                {due ? 'Change deadline…' : 'Set a deadline…'}
                <Icon name="chevronRight" size={15} />
              </button>
              <button type="button" role="menuitem" onClick={onNoteEdit}>
                {it.note ? 'Edit note' : 'Add a note'}
              </button>
              <button type="button" role="menuitem" className="danger" disabled={busy} onClick={onRemove}>
                Remove
              </button>
            </>
          )}
          {menu === 'move' && (
            <>
              <button type="button" role="menuitem" onClick={() => onMove(addDays(today, 1))}>
                Tomorrow
              </button>
              <button type="button" role="menuitem" onClick={() => onMove(addDays(today, 2))}>
                In 2 days
              </button>
              <button type="button" role="menuitem" onClick={() => onMove(nextMonday(today))}>
                Next Monday
              </button>
              <label className="hm-menu-pick">
                Pick a date
                <span>
                  <input type="date" className="hm-field" value={pick} min={today} onChange={(e) => setPick(e.target.value)} />
                  <Button size="sm" disabled={!pick} onClick={() => pick && onMove(pick)}>
                    Move
                  </Button>
                </span>
              </label>
            </>
          )}
          {menu === 'deadline' && (
            <>
              <label className="hm-menu-pick">
                Deadline
                <span>
                  <input type="date" className="hm-field" value={pick || due || ''} onChange={(e) => setPick(e.target.value)} />
                  <Button size="sm" disabled={!(pick || due)} onClick={() => onDeadline(pick || due || null)}>
                    Set
                  </Button>
                </span>
              </label>
              {due && (
                <button type="button" role="menuitem" onClick={() => onDeadline(null)}>
                  Clear the deadline
                </button>
              )}
            </>
          )}
        </div>
      )}
    </li>
  )
}
