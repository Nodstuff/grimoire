// Inline notices. window.alert/confirm are silent no-ops in Tauri's WKWebView,
// so every "tell the human something" path goes through notify(): a
// module-level queue rendered once by <Notices/> at the App root. Errors stay
// until clicked; ok notices auto-dismiss.

import { useEffect, useState } from 'react'

export type NoticeKind = 'error' | 'ok'

export interface Notice {
  id: number
  message: string
  kind: NoticeKind
}

const OK_TTL_MS = 4000

let seq = 0
let notices: Notice[] = []
const listeners = new Set<(n: Notice[]) => void>()

function emit() {
  for (const l of listeners) l(notices)
}

/** Format anything thrown into a message; api() throws Error(server.error). */
export function errText(e: unknown): string {
  if (e instanceof Error) return e.message
  return String(e).replace(/^Error:\s*/, '')
}

export function notify(message: string, kind: NoticeKind = 'error'): number {
  const id = ++seq
  notices = [...notices, { id, message: message.replace(/^Error:\s*/, ''), kind }]
  emit()
  if (kind === 'ok') setTimeout(() => dismiss(id), OK_TTL_MS)
  return id
}

export function dismiss(id: number) {
  if (!notices.some((n) => n.id === id)) return
  notices = notices.filter((n) => n.id !== id)
  emit()
}

/** Test/debug hook: current queue. */
export function currentNotices(): Notice[] {
  return notices
}

export function Notices() {
  const [list, setList] = useState<Notice[]>(notices)
  useEffect(() => {
    listeners.add(setList)
    setList(notices)
    return () => {
      listeners.delete(setList)
    }
  }, [])
  if (list.length === 0) return null
  return (
    <div className="notices" role="status" aria-live="polite">
      {list.map((n) => (
        <div
          key={n.id}
          className={`notice notice-${n.kind}`}
          onClick={() => dismiss(n.id)}
          title={n.kind === 'error' ? 'click to dismiss' : undefined}
        >
          {n.message}
        </div>
      ))}
    </div>
  )
}
