// Shared by Home's Today card (TodoList.tsx) and the To-dos screen
// (Todos.tsx): the live `due …` reading of a draft. The grammar lives only
// in the daemon (`due.rs`, `GET /api/todo/parse`); this asks it, debounced.

import { useEffect, useState } from 'react'
import { api } from '../types'
import { clientClock, hasDuePhrase, type DueParse } from '../todo'

export const JSON_HEADERS = { 'Content-Type': 'application/json' }

/** What the daemon makes of `draft`'s trailing `due …` / `by …` phrase, or
 * null when it has none (or the daemon cannot say). */
export function useDuePreview(draft: string, delayMs = 150): DueParse | null {
  const [preview, setPreview] = useState<DueParse | null>(null)
  useEffect(() => {
    if (!hasDuePhrase(draft)) {
      setPreview(null)
      return
    }
    let live = true
    const t = setTimeout(() => {
      const c = clientClock()
      api<DueParse>(`/api/todo/parse?text=${encodeURIComponent(draft.trim())}&today=${c.today}&utc_offset=${encodeURIComponent(c.utc_offset)}`)
        .then((r) => live && setPreview(r))
        .catch(() => live && setPreview(null))
    }, delayMs)
    return () => {
      live = false
      clearTimeout(t)
    }
  }, [draft, delayMs])
  return preview
}

/** POST a to-do write with the client's clock (the daemon has none for
 * you); answers the fresh day of the list it wrote. */
export function postTodo<T = unknown>(path: string, body: Record<string, unknown>): Promise<T> {
  return api<T>(path, { method: 'POST', headers: JSON_HEADERS, body: JSON.stringify({ ...clientClock(), ...body }) })
}
