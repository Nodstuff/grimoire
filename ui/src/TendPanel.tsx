// Tending (per-doc agent config): agents are OPT-IN per doc/folder. This
// panel lives on the doc — attach a scribe (writes the missing docs from a
// repo, imitating style exemplars), a keeper (keeps the scope true to the
// repo), or an auditor (flags/corrects stale claims), each with its own
// instructions, sources, and cadence.

import { useCallback, useEffect, useState } from 'react'
import { api, Doc, GardenerRun } from './types'
import { notify } from './Notice'

interface Tending {
  id: string
  name: string
  kind: 'scribe' | 'keeper' | 'auditor' | 'tagging' | 'reviewer'
  scope_doc: string | null
  scope_title: string
  inherited: boolean
  task_prompt: string
  bindings: { repos?: string[]; style_docs?: string[] } | unknown[]
  schedule: string
  confidence_policy: 'review' | 'gate'
  enabled: boolean
}

const KIND_INFO: Record<string, string> = {
  scribe: 'writes the missing docs from the sources, imitating your style exemplars',
  keeper: 'keeps these docs true to the sources — drift becomes reviewable fixes',
  auditor: 'sweeps for stale or wrong claims — verified fixes apply, suspicions park',
}

function bindingsOf(t: Tending): { repos: string[]; style_docs: string[] } {
  if (Array.isArray(t.bindings)) return { repos: t.bindings as string[], style_docs: [] }
  const b = t.bindings as { repos?: string[]; style_docs?: string[] }
  return { repos: b.repos ?? [], style_docs: b.style_docs ?? [] }
}

export default function TendPanel({
  doc,
  onClose,
  dataVersion,
}: {
  doc: Doc
  onClose: () => void
  dataVersion: number
}) {
  const [tendings, setTendings] = useState<Tending[]>([])
  const [runs, setRuns] = useState<GardenerRun[]>([])
  const [adding, setAdding] = useState(false)
  const [running, setRunning] = useState<string | null>(null)

  const load = useCallback(() => {
    api<Tending[]>(`/api/doc/${doc.id}/tendings`).then(setTendings).catch(console.error)
    api<GardenerRun[]>('/api/runs').then(setRuns).catch(() => {})
  }, [doc.id])

  useEffect(load, [load, dataVersion])

  const runNow = async (name: string) => {
    setRunning(name)
    await api('/admin/garden', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }),
    }).catch((e) => notify(String(e)))
    setRunning(null)
    load()
  }

  return (
    <aside className="panel tend-panel">
      <div className="panel-head">
        <span>tending — {doc.title}</span>
        <button onClick={onClose}>esc</button>
      </div>
      {tendings.length === 0 && !adding && (
        <div className="tend-empty">
          <p>
            This doc is <b>manual-only</b> — no agent touches it or anything inside it.
          </p>
          <p className="meta">
            Attach a tending to opt this {`doc/folder`} into agent care. Everything an agent
            does lands in the review queue with provenance.
          </p>
        </div>
      )}
      {tendings.map((t) => (
        <TendingCard
          key={t.id}
          t={t}
          lastRun={runs.find((r) => r.gardener_name === t.name)}
          running={running === t.name}
          onRun={() => runNow(t.name)}
          onSaved={load}
        />
      ))}
      {adding ? (
        <NewTending docId={doc.id} docTitle={doc.title} onDone={() => {
          setAdding(false)
          load()
        }} />
      ) : (
        <button className="chip tend-add" onClick={() => setAdding(true)}>
          + attach a tending
        </button>
      )}
    </aside>
  )
}

function TendingCard({
  t,
  lastRun,
  running,
  onRun,
  onSaved,
}: {
  t: Tending
  lastRun?: GardenerRun
  running: boolean
  onRun: () => void
  onSaved: () => void
}) {
  const b = bindingsOf(t)
  const [prompt, setPrompt] = useState(t.task_prompt)
  const [repos, setRepos] = useState(b.repos.join(', '))
  const [styleDocs, setStyleDocs] = useState(b.style_docs.join(', '))
  const [schedule, setSchedule] = useState(t.schedule)
  const [enabled, setEnabled] = useState(t.enabled)
  const dirty =
    prompt !== t.task_prompt ||
    repos !== b.repos.join(', ') ||
    styleDocs !== b.style_docs.join(', ') ||
    schedule !== t.schedule ||
    enabled !== t.enabled

  const save = async () => {
    await api('/admin/gardeners/update', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        id: t.id,
        task_prompt: prompt,
        schedule,
        confidence_policy: t.confidence_policy,
        scope_doc: t.scope_doc,
        enabled,
        bindings: {
          repos: repos.split(',').map((s) => s.trim()).filter(Boolean),
          style_docs: styleDocs.split(',').map((s) => s.trim()).filter(Boolean),
        },
      }),
    }).catch((e) => notify(String(e)))
    onSaved()
  }

  return (
    <div className={`tending ${enabled ? '' : 'disabled'}`}>
      <div className="tending-head">
        <span className={`kind-badge kind-${t.kind}`}>{t.kind}</span>
        {t.inherited && <span className="meta">inherited from {t.scope_title}</span>}
        <label className="toggle">
          <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
        </label>
      </div>
      <div className="meta kind-blurb">{KIND_INFO[t.kind] ?? ''}</div>
      <textarea
        className="prompt-edit"
        rows={3}
        value={prompt}
        onChange={(e) => setPrompt(e.target.value)}
        placeholder="instructions: how things are done here, where to look, what matters"
      />
      <label className="tend-field">
        sources (repo paths)
        <input value={repos} onChange={(e) => setRepos(e.target.value)} placeholder="/path/to/repo" />
      </label>
      {t.kind === 'scribe' && (
        <label className="tend-field">
          style exemplars (doc titles)
          <input
            value={styleDocs}
            onChange={(e) => setStyleDocs(e.target.value)}
            placeholder="Architecture, Review Gate"
          />
        </label>
      )}
      <div className="tending-foot">
        <select value={schedule} onChange={(e) => setSchedule(e.target.value)}>
          <option value="manual">manual only</option>
          <option value="daily">daily at 16:00</option>
        </select>
        {dirty && (
          <button className="accept" onClick={save}>
            save
          </button>
        )}
        <button className="chip" disabled={running || !enabled} onClick={onRun}>
          {running ? 'running…' : 'run now'}
        </button>
      </div>
      {lastRun && (
        <div className={`tend-lastrun ${lastRun.status}`}>
          <span className={`status ${lastRun.status}`}>{lastRun.status}</span>
          <span className="meta">{lastRun.started_at.slice(0, 16).replace('T', ' ')}</span>
          <pre>{(lastRun.summary ?? '').split('\n').slice(0, 4).join('\n')}</pre>
        </div>
      )}
    </div>
  )
}

function NewTending({
  docId,
  docTitle,
  onDone,
}: {
  docId: string
  docTitle: string
  onDone: () => void
}) {
  const [kind, setKind] = useState<'scribe' | 'keeper' | 'auditor'>('keeper')
  const [prompt, setPrompt] = useState('')
  const [repos, setRepos] = useState('')
  const [styleDocs, setStyleDocs] = useState('')

  const create = async () => {
    if (!prompt.trim()) return
    const name = `${kind}:${docTitle}`.slice(0, 60)
    const g = await api<{ id: string; error?: string }>('/admin/gardeners', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, kind, task_prompt: prompt.trim(), scope_doc: docId }),
    }).catch((e) => {
      notify(String(e))
      return null
    })
    if (!g) return
    await api('/admin/gardeners/update', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        id: g.id,
        task_prompt: prompt.trim(),
        schedule: 'manual',
        confidence_policy: 'review',
        scope_doc: docId,
        enabled: true,
        bindings: {
          repos: repos.split(',').map((s) => s.trim()).filter(Boolean),
          style_docs: styleDocs.split(',').map((s) => s.trim()).filter(Boolean),
        },
      }),
    }).catch((e) => notify(String(e)))
    onDone()
  }

  return (
    <div className="tending">
      <div className="tending-head">
        <select value={kind} onChange={(e) => setKind(e.target.value as typeof kind)}>
          <option value="scribe">scribe — write the missing docs</option>
          <option value="keeper">keeper — keep docs true to the sources</option>
          <option value="auditor">auditor — flag & fix stale claims</option>
        </select>
      </div>
      <div className="meta kind-blurb">{KIND_INFO[kind]}</div>
      <textarea
        className="prompt-edit"
        autoFocus
        rows={3}
        value={prompt}
        onChange={(e) => setPrompt(e.target.value)}
        placeholder="instructions: how this area is organized, conventions, where to find extra context…"
      />
      <label className="tend-field">
        sources (repo paths, comma-separated)
        <input value={repos} onChange={(e) => setRepos(e.target.value)} placeholder="/Users/you/code/repo" />
      </label>
      {kind === 'scribe' && (
        <label className="tend-field">
          style exemplars (doc titles to imitate)
          <input
            value={styleDocs}
            onChange={(e) => setStyleDocs(e.target.value)}
            placeholder="Architecture, Review Gate"
          />
        </label>
      )}
      <div className="tending-foot">
        <span className="meta">starts manual-only; runs land in review</span>
        <button className="accept" onClick={create}>
          attach
        </button>
        <button className="chip" onClick={onDone}>
          cancel
        </button>
      </div>
    </div>
  )
}
