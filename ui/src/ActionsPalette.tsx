// ⌘K Actions: commands only (commands.ts). With a doc open, its actions list
// first under "This doc". Typing filters fuzzily (omni.ts rankCommands).
// Every row shows its shortcut. No docs, no places, no search results:
// those are ⌘O and ⌘⇧F.

import { useEffect, useMemo, useRef, useState } from 'react'
import PaletteShell, { PaletteFooter, PaletteInput, moveSelection } from './PaletteShell'
import { Icon } from './lapis/icons'
import { rankCommands, type OmniCommand } from './omni'

export default function ActionsPalette({
  commands,
  onClose,
  docTitle,
}: {
  commands: OmniCommand[]
  onClose: () => void
  /** the open doc: shown at the input's right, its actions list first */
  docTitle?: string | null
}) {
  const [raw, setRaw] = useState('')
  const [sel, setSel] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  useEffect(() => inputRef.current?.focus(), [])

  const rows = useMemo(() => rankCommands(commands, raw), [commands, raw])
  const typed = raw.trim().length > 0
  useEffect(() => setSel(0), [raw])
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>(`[data-idx="${sel}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [sel])

  // the heading a row lists under, when nothing is typed
  const groupOf = (c: OmniCommand) => (c.scope === 'doc' ? 'This doc' : (c.group ?? 'Actions'))

  return (
    <PaletteShell onClose={onClose} className="palette-actions" label="Actions">
      <PaletteInput
        icon="actions"
        ref={inputRef}
        placeholder="Run an action…"
        aria-label="Run an action"
        role="combobox"
        aria-expanded="true"
        aria-controls="actions-list"
        value={raw}
        onChange={(e) => setRaw(e.target.value)}
        onKeyDown={(e) => {
          if (moveSelection(e, rows.length, setSel)) return
          if (e.key === 'Enter') {
            e.preventDefault()
            rows[sel]?.run()
          }
        }}
        right={
          docTitle ? (
            <span className="palette-ctx" title={`Acting on ${docTitle}`}>
              <Icon name="doc" size={13} />
              {docTitle}
            </span>
          ) : undefined
        }
      />
      <div className="palette-list omni-list" ref={listRef} role="listbox" id="actions-list" aria-label="Actions">
        {rows.length === 0 && (
          <div className="palette-empty">
            No such action — docs and places like Trash or Settings are in <kbd>⌘O</kbd>
          </div>
        )}
        {rows.map((c, i) => {
          const g = groupOf(c)
          const head = !typed && (i === 0 || groupOf(rows[i - 1]) !== g)
          return (
            <div key={c.id}>
              {head && i > 0 && <div className="omni-sep" />}
              {head && <div className="omni-group">{g}</div>}
              <div
                data-idx={i}
                role="option"
                aria-selected={i === sel}
                className={`palette-item ${i === sel ? 'sel' : ''}`}
                onMouseMove={() => sel !== i && setSel(i)}
                onClick={() => c.run()}
              >
                <Icon name={c.icon ?? 'actions'} size={15} />
                <span className="hit-body">
                  <span>{c.label}</span>
                </span>
                <span className="hint">{c.keys ? <kbd>{c.keys}</kbd> : c.hint && <span className="hit-text">{c.hint}</span>}</span>
              </div>
            </div>
          )
        })}
      </div>
      <PaletteFooter
        hints={[['↵', 'run']]}
        end={
          <>
            Places and docs live in <kbd>⌘O</kbd>
          </>
        }
      />
    </PaletteShell>
  )
}
