import { useEffect, useRef, useState } from 'react'
import PaletteShell, { PaletteInput } from './PaletteShell'
import { notify } from './Notice'
import { api, type Doc } from './types'

export default function NewDocPalette({ onCreated, onClose }: { onCreated: (id: string) => void; onClose: () => void }) {
  const [title, setTitle] = useState('')
  const inputRef = useRef<HTMLInputElement>(null)
  useEffect(() => inputRef.current?.focus(), [])

  const create = async () => {
    if (!title.trim()) return
    const d = await api<Doc>('/api/docs', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ title: title.trim() }),
    })
    onCreated(d.id)
  }

  return (
    <PaletteShell onClose={onClose}>
      <PaletteInput
        icon="docPlus"
        ref={inputRef}
        aria-label="New doc title"
        placeholder="New doc title…"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') create().catch((err) => notify(String(err)))
        }}
      />
      <div className="palette-empty">Enter creates it at the top level and opens it</div>
    </PaletteShell>
  )
}
