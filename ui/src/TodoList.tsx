// The home's To-do section: today's items out of the daemon's per-day list
// (`/api/todo`, one `## YYYY-MM-DD` heading per day in the To-do doc). A
// checklist with comfortable hit targets, an inline add row (Enter adds, Esc
// clears), click-to-edit text, a hover ⋯ menu (move / deadline / note), a
// deadline pill on the right, the note as a muted preview with a disclosure,
// and a read-only look at the previous day. Every write is a POST that
// answers with the fresh day, so the list never guesses.

import { useCallback, useEffect, useRef, useState } from 'react'
import { errText, notify } from './Notice'
import { api } from './types'
import { addDays, deadlineTone, fmtDay, fmtDeadline, nextMonday, type TodoDay, type TodoItem } from './todo'

const JSON_HEADERS = { 'Content-Type': 'application/json' }

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

  const apply = useCallback(
    (d: TodoDay | null) => {
      setDay(d)
      onLoaded?.(d)
    },
    [onLoaded],
  )

  const load = useCallback(() => {
    api<TodoDay>(`/api/todo?date=${encodeURIComponent(date)}`)
      .then((d) => {
        setMissing(false)
        apply(d)
        if (d.carried > 0) notify(`${d.carried} to-do${d.carried === 1 ? '' : 's'} carried forward from ${d.prev_date}`, 'ok')
      })
      .catch((e) => {
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
      const d = await api<TodoDay>(path, { method: 'POST', headers: JSON_HEADERS, body: JSON.stringify({ date, ...body }) })
      apply(d)
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
      if (!(e.target as HTMLElement).closest('.todo-menu, .todo-more')) setMenu(null)
    }
    document.addEventListener('keydown', onKey, true)
    document.addEventListener('mousedown', onClick)
    return () => {
      document.removeEventListener('keydown', onKey, true)
      document.removeEventListener('mousedown', onClick)
    }
  }, [menu])

  if (missing) return <div className="home-empty">this Grimoire is older than the to-do list — update it</div>
  if (!day) return <div className="home-empty todo-loading">…</div>

  const today = day.today
  const items = day.items
  const toggleNote = (id: string) =>
    setNoteOpen((s) => {
      const n = new Set(s)
      if (n.has(id)) n.delete(id)
      else n.add(id)
      return n
    })

  return (
    <div className={`todo ${readOnly ? 'readonly' : ''}`}>
      {items.length === 0 && (
        <div className="home-empty">{readOnly ? `nothing on ${fmtDay(date, today)}` : 'nothing yet — add one below'}</div>
      )}
      <ul className="todo-list">
        {items.map((it) => (
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
              post('/api/todo/move', { item_id: it.id, to_date: to }).then((ok) => ok && notify(`moved to ${fmtDay(to, today)}`, 'ok'))
            }}
            onDeadline={(dl) => {
              setMenu(null)
              post('/api/todo/deadline', { item_id: it.id, deadline: dl })
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
        ))}
      </ul>
      {!readOnly && (
        <div className="todo-add">
          <span className="todo-add-mark" aria-hidden>
            +
          </span>
          <input
            className="todo-add-input"
            placeholder="add a to-do · Enter adds · ⏰ 2026-09-20 sets a deadline"
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
        </div>
      )}
      <div className="todo-foot">
        <button className="home-link dim" onClick={() => onOpenDoc(day.doc_id)}>
          open the To-do doc →
        </button>
      </div>
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

  const tone = deadlineTone(it)
  const state = it.done ? 'done' : it.carried ? 'carried' : 'open'
  const notePreview = it.note?.split('\n')[0] ?? ''

  return (
    <li className={`todo-item ${state} ${menu ? 'menu-open' : ''}`}>
      <div className="todo-main">
        <button
          className={`todo-box ${state}`}
          role="checkbox"
          aria-checked={it.done}
          disabled={busy || readOnly || !!it.carried}
          title={it.carried ? 'moved to a later day' : it.done ? 'mark not done' : 'mark done'}
          onClick={onToggle}
        >
          {it.done ? '✓' : it.carried ? '›' : ''}
        </button>
        {editing ? (
          <input
            ref={editRef}
            className="todo-edit"
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
          <span className="todo-text" onClick={onEditStart} title={readOnly ? undefined : 'click to edit'}>
            {it.text}
            {it.carried_from && <span className="todo-carried">from {fmtDay(it.carried_from, today)}</span>}
          </span>
        )}
        {it.note && !editing && (
          <button className={`todo-note-toggle ${noteOpen ? 'on' : ''}`} title={noteOpen ? 'hide note' : 'show note'} onClick={onNoteToggle}>
            ≡
          </button>
        )}
        {it.deadline && (
          <span className={`todo-deadline ${tone}`} title={`deadline ${it.deadline}`}>
            {fmtDeadline(it.deadline, today)}
          </span>
        )}
        {!readOnly && (
          <span className="todo-actions">
            <button className="todo-more" title="move · deadline · note" onClick={() => onMenu(menu ? null : 'root')}>
              ⋯
            </button>
            <button className="todo-remove" title="remove" disabled={busy} onClick={onRemove}>
              ×
            </button>
          </span>
        )}
      </div>
      {it.note && noteOpen && !noteEditing && (
        <div className="todo-note" onClick={readOnly ? undefined : onNoteEdit} title={readOnly ? undefined : 'click to edit the note'}>
          {it.note}
        </div>
      )}
      {it.note && !noteOpen && !noteEditing && (
        <div className="todo-note-preview" onClick={onNoteToggle}>
          {notePreview}
        </div>
      )}
      {noteEditing && (
        <textarea
          ref={noteRef}
          className="todo-note-edit"
          rows={Math.min(6, Math.max(2, note.split('\n').length + 1))}
          placeholder="a note under the item — ⌘Enter saves, Esc cancels"
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
        <div className="todo-menu" role="menu">
          {menu === 'root' && (
            <>
              <button onClick={() => onMenu('move')}>move →</button>
              <button onClick={() => onMenu('deadline')}>{it.deadline ? 'deadline →' : 'set a deadline →'}</button>
              <button onClick={onNoteEdit}>{it.note ? 'edit note' : 'add a note'}</button>
            </>
          )}
          {menu === 'move' && (
            <>
              <button onClick={() => onMove(addDays(today, 1))}>tomorrow</button>
              <button onClick={() => onMove(addDays(today, 2))}>+2 days</button>
              <button onClick={() => onMove(nextMonday(today))}>next Monday</button>
              <label className="todo-menu-pick">
                pick a date
                <input type="date" value={pick} min={today} onChange={(e) => setPick(e.target.value)} />
                <button disabled={!pick} onClick={() => pick && onMove(pick)}>
                  move
                </button>
              </label>
            </>
          )}
          {menu === 'deadline' && (
            <>
              <label className="todo-menu-pick">
                deadline
                <input type="date" value={pick || it.deadline || ''} onChange={(e) => setPick(e.target.value)} />
                <button disabled={!(pick || it.deadline)} onClick={() => onDeadline(pick || it.deadline || null)}>
                  set
                </button>
              </label>
              {it.deadline && <button onClick={() => onDeadline(null)}>clear the deadline</button>}
            </>
          )}
        </div>
      )}
    </li>
  )
}
