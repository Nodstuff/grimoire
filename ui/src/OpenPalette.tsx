// ⌘O Open: one input over doc titles, their breadcrumb paths, and places
// (Home, Books, Review, Gardeners, Graph, Trash, Settings, E-readers). Empty →
// recent docs (not the one open), then places. Typed → one fuzzy-ranked list (omni.ts
// buildOpenRows). Nothing else: no actions (⌘K), no content hits (⌘⇧F).
// Enter opens; ⌘Enter hands the typed text to Search.

import { useDeferredValue, useEffect, useMemo, useRef, useState } from 'react'
import PaletteShell, { PaletteFooter, PaletteInput, moveSelection } from './PaletteShell'
import { Icon } from './lapis/icons'
import type { Doc } from './types'
import type { OpenDoc } from './App'
import { buildOpenRows, loadRecentIds, type OpenRow, type Place, type PlaceId } from './omni'

export default function OpenPalette({
  docs,
  places,
  currentId = null,
  onOpenDoc,
  onPlace,
  onSearchText,
  onClose,
}: {
  docs: Doc[]
  places: Place[]
  /** the doc on screen, left out of Recent */
  currentId?: string | null
  onOpenDoc: OpenDoc
  onPlace: (id: PlaceId) => void
  /** ⌘Enter: search inside docs for what was typed */
  onSearchText: (q: string) => void
  onClose: () => void
}) {
  const [raw, setRaw] = useState('')
  // titles rank locally: no debounce, but a big library never blocks a keystroke
  const q = useDeferredValue(raw)
  const [sel, setSel] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  useEffect(() => inputRef.current?.focus(), [])
  const recentIds = useMemo(loadRecentIds, [])

  const rows = useMemo(() => buildOpenRows({ q, docs, places, recentIds, currentId }), [q, docs, places, recentIds, currentId])
  const typed = q.trim().length > 0
  const recentCount = typed ? 0 : rows.filter((r) => r.kind === 'doc').length
  useEffect(() => setSel(0), [q])
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>(`[data-idx="${sel}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [sel])

  const run = (r: OpenRow | undefined) => {
    if (!r) return
    if (r.kind === 'doc') onOpenDoc(r.doc.id)
    else onPlace(r.place.id)
  }

  const item = (r: OpenRow, i: number) => (
    <div
      key={r.key}
      data-idx={i}
      role="option"
      aria-selected={i === sel}
      className={`palette-item ${i === sel ? 'sel' : ''}`}
      // mousemove, not mouseenter: a list re-flowing under a
      // stationary pointer must not steal the selection
      onMouseMove={() => sel !== i && setSel(i)}
      onClick={() => run(r)}
    >
      {r.kind === 'doc' ? (
        <>
          <span className="omni-ico">
            <Icon name="doc" size={15} />
          </span>
          <span className="hit-body">
            <span className="hit-title">{r.doc.title}</span>
            {r.path !== r.doc.title && <span className="hit-text">{r.path.split(' › ').slice(0, -1).join(' › ')}</span>}
          </span>
          <span className="hint">
            {typed && r.recent ? 'recent' : ''}
            {i === sel && <kbd>↵</kbd>}
          </span>
        </>
      ) : typed ? (
        <>
          <span className="omni-ico">
            <Icon name={r.place.icon ?? 'home'} size={15} />
          </span>
          <span className="hit-body">
            <span className="hit-title">{r.place.label}</span>
            <span className="hit-text">{r.place.hint}</span>
          </span>
          <span className="hint">{r.place.keys ? <kbd>{r.place.keys}</kbd> : 'place'}</span>
        </>
      ) : (
        <>
          <Icon name={r.place.icon ?? 'home'} size={15} />
          <span className="omni-place-label" title={r.place.hint}>
            {r.place.label}
          </span>
          {r.place.keys && (
            <span className="hint">
              <kbd>{r.place.keys}</kbd>
            </span>
          )}
        </>
      )}
    </div>
  )

  return (
    <PaletteShell onClose={onClose} label="Open a doc or place">
      <PaletteInput
        icon="search"
        ref={inputRef}
        placeholder="Open a doc or place…"
        aria-label="Open a doc or place"
        role="combobox"
        aria-expanded="true"
        aria-controls="open-list"
        value={raw}
        onChange={(e) => setRaw(e.target.value)}
        onKeyDown={(e) => {
          if (moveSelection(e, rows.length, setSel)) return
          if (e.key === 'Enter') {
            e.preventDefault()
            if (e.metaKey || e.ctrlKey) {
              if (raw.trim()) onSearchText(raw.trim())
            } else run(rows[sel])
          }
        }}
        right={<kbd>esc</kbd>}
      />
      <div className="palette-list omni-list" ref={listRef} role="listbox" id="open-list" aria-label="Results">
        {rows.length === 0 && (
          <div className="palette-empty">
            {typed ? (
              <>
                No titles match — <kbd>⌘↵</kbd>searches inside docs
              </>
            ) : (
              'No docs yet — ⌘⌥N creates one'
            )}
          </div>
        )}
        {typed ? (
          rows.map(item)
        ) : (
          <>
            {recentCount > 0 && <div className="omni-group">Recent</div>}
            {rows.slice(0, recentCount).map(item)}
            {rows.length > recentCount && (
              <>
                {recentCount > 0 && <div className="omni-sep" />}
                <div className="omni-group">Places</div>
                <div className="omni-places">{rows.slice(recentCount).map((r, j) => item(r, recentCount + j))}</div>
              </>
            )}
          </>
        )}
      </div>
      <PaletteFooter
        hints={[
          ['↑↓', 'move'],
          ['↵', 'open'],
          ['⌘↵', 'search inside docs'],
        ]}
        end={
          <>
            <kbd>⌘K</kbd>actions
          </>
        }
      />
    </PaletteShell>
  )
}
