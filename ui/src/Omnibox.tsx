// The omnibox: one palette for open (⌘O), search (⌘P/⌘F), ask (⌘/) and
// commands (⌘K). Empty → recent docs + top commands; typing → Docs, Content,
// Commands and a trailing "Ask the vault" row, walked as one list by the
// arrow keys. `>` = commands only, `?` = ask only. ⌘Enter asks from anywhere.
// Same chrome as every palette (PaletteShell); the pure mixing lives in
// omni.ts.

import { useEffect, useMemo, useRef, useState } from 'react'
import PaletteShell from './PaletteShell'
import { errText, notify } from './Notice'
import { api, type Doc, type SearchHit } from './types'
import type { OpenDoc } from './App'
import {
  GROUP_LABEL,
  buildRows,
  groupStarts,
  loadRecentIds,
  parseQuery,
  placeholderFor,
  type OmniCommand,
  type OmniMode,
  type OmniRow,
} from './omni'

export default function Omnibox({
  mode: baseMode,
  docs,
  commands,
  onOpenDoc,
  onClose,
}: {
  /** how the box was opened: ⌘K mixed, ⌘O docs, ⌘P content, ⌘/ ask */
  mode: OmniMode
  docs: Doc[]
  commands: OmniCommand[]
  onOpenDoc: OpenDoc
  onClose: () => void
}) {
  const [raw, setRaw] = useState('')
  const [hits, setHits] = useState<SearchHit[]>([])
  const [sel, setSel] = useState(0)
  const [asking, setAsking] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  useEffect(() => inputRef.current?.focus(), [])

  const { mode, q } = parseQuery(raw, baseMode)
  const wantsContent = (mode === 'mixed' || mode === 'content') && q.length >= 2

  // content hits: debounced, latest wins (a slow older reply must not land
  // over the hits for what is in the box now)
  useEffect(() => {
    if (!wantsContent) {
      setHits([])
      return
    }
    let stale = false
    const t = setTimeout(() => {
      api<SearchHit[]>(`/api/search?q=${encodeURIComponent(q)}`)
        .then((hs) => {
          if (!stale) setHits(Array.isArray(hs) ? hs : [])
        })
        .catch(() => {
          if (!stale) setHits([])
        })
    }, 120)
    return () => {
      stale = true
      clearTimeout(t)
    }
  }, [q, wantsContent])

  const recentIds = useMemo(loadRecentIds, [])
  const rows: OmniRow[] = useMemo(
    () => buildRows({ mode, q, docs, recentIds, hits, commands }),
    [mode, q, docs, recentIds, hits, commands],
  )
  const starts = useMemo(() => groupStarts(rows), [rows])
  useEffect(() => setSel(0), [q, mode])
  useEffect(() => {
    setSel((s) => Math.min(s, Math.max(rows.length - 1, 0)))
  }, [rows.length])
  // keep the selected row in view as the arrows move
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-idx="${sel}"]`)
    el?.scrollIntoView({ block: 'nearest' })
  }, [sel])

  const ask = async (question: string) => {
    const qq = question.trim()
    if (!qq || asking) return
    setAsking(true)
    try {
      const a = await api<{ doc_id: string | null; title: string; sources: number; docs: number }>('/api/ask', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ question: qq }),
      })
      if (!a.doc_id) {
        notify('nothing in your notes matches that yet', 'warn')
        setAsking(false)
        return
      }
      notify(`answered from ${a.sources} block${a.sources === 1 ? '' : 's'} across ${a.docs} doc${a.docs === 1 ? '' : 's'}`, 'ok')
      onOpenDoc(a.doc_id)
    } catch (e) {
      notify(errText(e))
      setAsking(false)
    }
  }

  const run = (r: OmniRow | undefined) => {
    if (!r) return
    switch (r.group) {
      case 'recent':
      case 'docs':
        onOpenDoc(r.doc.id)
        break
      case 'content':
        onOpenDoc(r.hit.block.doc_id, { blockId: r.hit.block.id })
        break
      case 'commands':
        r.cmd.run()
        break
      case 'ask':
        ask(r.query || q)
        break
    }
  }

  const empty = rows.length === 0
  const emptyText =
    mode === 'commands'
      ? 'no such command'
      : mode === 'docs' && docs.length === 0
        ? 'no docs yet — ⌘N creates one'
        : q.length > 0
          ? 'no matches'
          : 'type to search docs and content · > for commands · ? to ask'

  return (
    <PaletteShell onClose={onClose} locked={asking}>
      <input
        ref={inputRef}
        placeholder={placeholderFor(baseMode)}
        value={raw}
        disabled={asking}
        onChange={(e) => setRaw(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown') {
            e.preventDefault()
            setSel((s) => Math.min(s + 1, rows.length - 1))
          } else if (e.key === 'ArrowUp') {
            e.preventDefault()
            setSel((s) => Math.max(s - 1, 0))
          } else if (e.key === 'Enter') {
            e.preventDefault()
            // ⌘Enter asks from anywhere in the list; Enter with nothing
            // selectable but a question also asks
            if (e.metaKey || e.ctrlKey || (empty && q)) ask(q)
            else run(rows[sel])
          }
        }}
      />
      <div className="palette-list omni-list" ref={listRef}>
        {asking && <div className="palette-empty">🌿 reading your notes and writing the answer… (up to a minute)</div>}
        {!asking && empty && <div className="palette-empty">{emptyText}</div>}
        {!asking &&
          rows.map((r, i) => (
            <div key={r.key}>
              {starts.has(i) && <div className="omni-group">{GROUP_LABEL[r.group]}</div>}
              <div
                data-idx={i}
                className={`palette-item omni-${r.group} ${i === sel ? 'sel' : ''}`}
                onMouseEnter={() => setSel(i)}
                onClick={() => run(r)}
              >
                {(r.group === 'recent' || r.group === 'docs') && (
                  <>
                    <span className="hit-body">
                      <span>{r.doc.is_canvas ? `▨ ${r.doc.title}` : r.doc.title}</span>
                      {r.path !== r.doc.title && <span className="hit-text">{r.path}</span>}
                    </span>
                    <span className="hint">{r.doc.is_canvas ? 'canvas' : i === sel && r.group === 'recent' ? 'recent' : 'doc'}</span>
                  </>
                )}
                {r.group === 'content' && (
                  <div className="hit-body">
                    <span className="hit-doc">{r.crumb}</span>
                    <span className="hit-text">{r.snippet}</span>
                  </div>
                )}
                {r.group === 'commands' && (
                  <>
                    <span>{r.cmd.label}</span>
                    <span className="hint">{i === sel && r.cmd.keys ? r.cmd.keys : r.cmd.hint ?? (r.cmd.keys ?? '')}</span>
                  </>
                )}
                {r.group === 'ask' && (
                  <>
                    <span>
                      Ask the vault: <em>“{r.query || '…'}”</em>
                    </span>
                    <span className="hint">{i === sel ? '↵ · ⌘↵ from anywhere' : 'an answer doc with citations'}</span>
                  </>
                )}
              </div>
            </div>
          ))}
      </div>
    </PaletteShell>
  )
}
