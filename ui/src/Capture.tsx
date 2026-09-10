// Quick capture (⌘⇧I, the ⌥⌘G global hotkey, tray → Quick capture): one
// textarea, Enter files the note under Inbox as you (POST /api/inbox),
// Shift+Enter adds a line, Esc closes. The toast links to the new doc.
// Works the same in a browser tab.

import { useEffect, useRef, useState } from 'react'
import PaletteShell from './PaletteShell'
import { errText, notify } from './Notice'

import { api } from './types'

export interface Captured {
  doc_id: string
  title: string
  inbox_id: string
}

export default function Capture({
  onClose,
  onCaptured,
}: {
  onClose: () => void
  /** the note landed: the caller toasts (with a link) and refreshes docs */
  onCaptured: (c: Captured) => void
}) {
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)
  const ref = useRef<HTMLTextAreaElement>(null)
  useEffect(() => ref.current?.focus(), [])

  const save = async () => {
    const body = text.trim()
    if (!body || busy) return
    setBusy(true)
    try {
      const c = await api<Captured>('/api/inbox', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ text: body }),
      })
      onCaptured(c)
      onClose()
    } catch (e) {
      notify(errText(e))
      setBusy(false)
    }
  }

  return (
    <PaletteShell onClose={onClose} locked={busy}>
      <div className="capture">
        <textarea
          ref={ref}
          className="capture-text"
          placeholder="Capture a thought… Enter saves to Inbox, Shift+Enter for a new line"
          value={text}
          disabled={busy}
          rows={4}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.preventDefault()
              onClose()
              return
            }
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault()
              save()
            }
          }}
        />
        <div className="capture-foot">
          <span className="home-dim">{busy ? 'saving…' : 'first line becomes the title · tagged inbox'}</span>
          <button className="home-cta" disabled={busy || !text.trim()} onClick={save}>
            capture ↵
          </button>
        </div>
      </div>
    </PaletteShell>
  )
}
