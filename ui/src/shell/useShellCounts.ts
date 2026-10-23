// The sidebar's count pills: Inbox (waiting captures), To-dos (open today),
// Trash. Each loads on its own, refreshes when the store stamp moves
// (dataVersion), and stays absent when its route is missing on an older
// daemon. The Review count is App's (it already polls it).

import { useEffect, useState } from 'react'
import { api } from '../types'
import { clientClock, openItems, type TodoDay } from '../todo'

export interface SideData {
  inbox: number | null
  inboxDoc: string | null
  todos: number | null
  todoDoc: string | null
  trash: number | null
}

const EMPTY: SideData = { inbox: null, inboxDoc: null, todos: null, todoDoc: null, trash: null }

export function useShellCounts(dataVersion: number): SideData {
  const [d, setD] = useState<SideData>(EMPTY)
  useEffect(() => {
    let live = true
    const today = clientClock().today
    api<{ doc_id: string | null; items: unknown[] }>('/api/inbox')
      .then((r) => live && setD((c) => ({ ...c, inbox: Array.isArray(r?.items) ? r.items.length : null, inboxDoc: r?.doc_id ?? null })))
      .catch(() => {})
    api<TodoDay>(`/api/todo?date=${today}&today=${today}`)
      .then((r) => live && setD((c) => ({ ...c, todos: Array.isArray(r?.items) ? openItems(r.items).length : null, todoDoc: r?.doc_id ?? null })))
      .catch(() => {})
    api<unknown[]>('/api/trash')
      .then((r) => live && setD((c) => ({ ...c, trash: Array.isArray(r) ? r.length : null })))
      .catch(() => {})
    return () => {
      live = false
    }
  }, [dataVersion])
  return d
}
