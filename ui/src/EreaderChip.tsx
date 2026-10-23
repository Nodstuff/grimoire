// The doc header's "On e-readers" chip: shown while the doc is a book in
// this person's e-reader feed ("On e-readers"), or a chapter of one
// ("On e-readers · in <Folder>"). Its menu stops sending or downloads the
// EPUB; Send itself lives in ⌘K (and the phone's ⋯), beside Download EPUB.

import { useEffect, useRef, useState } from 'react'
import { Chip } from './lapis'
import { errText, notify } from './Notice'
import { downloadEpub, ereaderChipText, setSending, type EreaderStatus } from './ereader'

export default function EreaderChip({
  docId,
  title,
  status,
  onOpenDoc,
}: {
  docId: string
  title: string
  status: EreaderStatus | null
  onOpenDoc: (id: string) => void
}) {
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState(false)
  const box = useRef<HTMLSpanElement>(null)
  useEffect(() => {
    if (!open) return
    const off = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', off)
    return () => document.removeEventListener('mousedown', off)
  }, [open])
  const text = ereaderChipText(status)
  if (!status || !text) return null
  const book = status.sent ? null : status.in_sent_book

  const stop = async () => {
    setOpen(false)
    setBusy(true)
    try {
      await setSending(docId, false)
      notify(`stopped sending “${title}”`, 'ok')
    } catch (e) {
      notify(errText(e))
    }
    setBusy(false)
  }
  const download = () => {
    setOpen(false)
    const id = book?.id ?? docId
    downloadEpub(id, book?.title ?? title)
      .then((name) => notify(`downloaded ${name}`, 'ok'))
      .catch((e) => notify(errText(e)))
  }

  return (
    <span className="dv-menu-wrap" ref={box}>
      <Chip
        tone="accent"
        icon="books"
        onClick={() => setOpen(!open)}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-busy={busy}
        title={status.sent ? `a book in your e-readers’ catalog · ${status.chapters} chapter${status.chapters === 1 ? '' : 's'}` : 'a chapter of a book on your e-readers'}
      >
        {text}
      </Chip>
      {open && (
        <div
          className="dv-menu"
          role="menu"
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.stopPropagation()
              setOpen(false)
            }
          }}
        >
          {book && (
            <button type="button" role="menuitem" autoFocus onClick={() => (setOpen(false), onOpenDoc(book.id))}>
              Open {book.title}
            </button>
          )}
          <button type="button" role="menuitem" autoFocus={!book} onClick={download}>
            {book ? 'Download the book’s EPUB' : 'Download EPUB'}
          </button>
          {status.sent && (
            <button type="button" role="menuitem" className="dv-menu-clear" disabled={busy} onClick={stop}>
              Stop sending to e-readers
            </button>
          )}
        </div>
      )}
    </span>
  )
}
