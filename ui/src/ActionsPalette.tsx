// ⌘K Actions: commands only (commands.ts). With a doc open, its actions list
// first under "This doc". Typing filters fuzzily (omni.ts rankCommands).
// Every row shows its shortcut. No docs, no places, no search results:
// those are ⌘O and ⌘⇧F.

import { useEffect, useMemo, useRef, useState } from 'react'
import PaletteShell, { PaletteFooter, moveSelection } from './PaletteShell'
import { rankCommands, type OmniCommand } from './omni'

export default function ActionsPalette({ commands, onClose }: { commands: OmniCommand[]; onClose: () => void }) {
  const [raw, setRaw] = useState('')
  const [sel, setSel] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  useEffect(() => inputRef.current?.focus(), [])

  const rows = useMemo(() => rankCommands(commands, raw), [commands, raw])
  const typed = raw.trim().length > 0
  const docCount = typed ? 0 : rows.filter((c) => c.scope === 'doc').length
  useEffect(() => setSel(0), [raw])
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>(`[data-idx="${sel}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [sel])

  return (
    <PaletteShell onClose={onClose}>
      <input
        ref={inputRef}
        placeholder="Run an action…"
        aria-label="Run an action"
        value={raw}
        onChange={(e) => setRaw(e.target.value)}
        onKeyDown={(e) => {
          if (moveSelection(e, rows.length, setSel)) return
          if (e.key === 'Enter') {
            e.preventDefault()
            rows[sel]?.run()
          }
        }}
      />
      <div className="palette-list omni-list" ref={listRef} role="listbox">
        {rows.length === 0 && (
          <div className="palette-empty">
            no such action — docs and places like Trash or Settings are in <kbd>⌘O</kbd>
          </div>
        )}
        {rows.map((c, i) => (
          <div key={c.id}>
            {docCount > 0 && i === 0 && <div className="omni-group">This doc</div>}
            {docCount > 0 && i === docCount && <div className="omni-group">Actions</div>}
            <div
              data-idx={i}
              role="option"
              aria-selected={i === sel}
              className={`palette-item ${i === sel ? 'sel' : ''}`}
              onMouseMove={() => sel !== i && setSel(i)}
              onClick={() => c.run()}
            >
              <span className="hit-body">
                <span>{c.label}</span>
                {c.hint && <span className="hit-text">{c.hint}</span>}
              </span>
              <span className="hint">{c.keys && <kbd>{c.keys}</kbd>}</span>
            </div>
          </div>
        ))}
      </div>
      <PaletteFooter
        hints={[
          ['↵', 'run'],
          ['⌘O', 'open'],
          ['⌘⇧F', 'search'],
          ['esc', 'close'],
        ]}
      />
    </PaletteShell>
  )
}
